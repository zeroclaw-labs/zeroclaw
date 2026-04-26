//! Matrix channel using matrix-rust-sdk 0.16.
//!
//! Organisation (single file, internal `mod` blocks):
//! - `markers`: parse `[image:...] [voice:...]` etc. from outbound text
//! - `mention`: detect `m.mentions.user_ids` + body fallback
//! - `allowlist`: filter inbound by sender + room
//! - `approval`: 8-char token gen + reply parser
//! - `context`: thread-root preamble fetcher + delivered-set
//! - `streaming`: Partial + MultiMessage state machines
//! - `session`: `session.json` blob persistence next to the SQLite store
//! - `client`: SDK build, login/restore, recovery, cross-signing bootstrap, alias resolve
//! - `inbound`: event handlers + sync loop
//! - `outbound`: Channel::send + reactions + redact + media upload
//!
//! All protocol details (E2EE, sync token, encrypted upload, edits, threads, recovery)
//! are delegated to the SDK. We only own user-facing config logic and small bits of
//! cross-cutting state.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use async_trait::async_trait;
use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock, mpsc, oneshot};

use matrix_sdk::{
    Client,
    ruma::{OwnedEventId, OwnedRoomId},
};

use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
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
        if allowed_users.is_empty() {
            return false;
        }
        if allowed_users.iter().any(|u| u == "*") {
            return true;
        }
        allowed_users.iter().any(|u| u == sender)
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

    use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId};

    pub(super) type DraftKey = (OwnedRoomId, Option<OwnedEventId>);

    #[derive(Debug, Clone)]
    pub(super) struct PartialDraft {
        pub event_id: OwnedEventId,
        pub last_text: String,
        pub last_edit: Instant,
    }

    #[derive(Debug, Clone)]
    pub(super) struct MultiDraft {
        pub current_event_id: OwnedEventId,
    }

    #[derive(Default, Debug)]
    pub(super) struct State {
        pub partial: HashMap<DraftKey, PartialDraft>,
        pub multi: HashMap<DraftKey, MultiDraft>,
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
        let bytes = std::fs::read(&p)
            .map_err(|e| anyhow::anyhow!("read matrix session blob {}: {e}", p.display()))?;
        let blob: SessionBlob = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("parse matrix session blob {}: {e}", p.display()))?;
        Ok(Some(blob))
    }

    pub(super) fn save(state_dir: &Path, blob: &SessionBlob) -> anyhow::Result<()> {
        std::fs::create_dir_all(state_dir)
            .map_err(|e| anyhow::anyhow!("create matrix state dir {}: {e}", state_dir.display()))?;
        let p = path(state_dir);
        let json = serde_json::to_vec_pretty(blob)?;
        std::fs::write(&p, json)
            .map_err(|e| anyhow::anyhow!("write matrix session blob {}: {e}", p.display()))?;
        Ok(())
    }
}

// ─── client ────────────────────────────────────────────────────────────────
mod client {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::Arc,
    };

    use anyhow::{Context as _, Result, anyhow, bail};
    use matrix_sdk::{
        Client, SessionMeta, SessionTokens,
        authentication::matrix::MatrixSession,
        ruma::{
            OwnedRoomId, RoomAliasId,
            api::client::uiaa::{AuthData, Password, UserIdentifier},
        },
    };
    use tokio::sync::RwLock;
    use tracing::{debug, info, warn};

    use super::session;
    use zeroclaw_config::schema::MatrixConfig;

    pub(super) fn store_dir(state_dir: &Path) -> PathBuf {
        state_dir.join("store")
    }

    /// Build the SDK client, handling all three of:
    /// - normal restore from a consistent session.json + store/
    /// - first-run fresh login
    /// - corruption recovery (with password)
    ///
    /// Corruption signals (per matrix-sdk encryption.md and SDK source —
    /// `manager.rs:237` for `SigningKeyChanged`, `mod.rs:683` for OTK
    /// duplicates): the SDK rejects a device key update when the store and
    /// server disagree, and offers no public API to selectively forget a
    /// device record. The official remediation is "Clear storage to create
    /// a new device". We do that automatically when password + user_id are
    /// configured; otherwise we surface a clear error so the operator can
    /// either provide a password or wipe state manually.
    ///
    /// Wrong-recovery-key failures are *not* a corruption signal — they're
    /// an operator-config issue. We log them clearly and continue with
    /// `bootstrap_cross_signing_if_needed`, which sets up fresh cross-signing
    /// when no identity could be imported.
    pub(super) async fn build(config: &MatrixConfig, state_dir: &Path) -> Result<Client> {
        build_attempt(config, state_dir, 0).await
    }

    fn wipe_state(state_dir: &Path) -> Result<()> {
        let session = session::path(state_dir);
        if session.exists()
            && let Err(e) = std::fs::remove_file(&session)
        {
            return Err(anyhow!(
                "matrix: failed to remove {} during corruption recovery: {e}. Fix permissions or wipe the directory manually.",
                session.display()
            ));
        }
        let store = store_dir(state_dir);
        if store.exists()
            && let Err(e) = std::fs::remove_dir_all(&store)
        {
            return Err(anyhow!(
                "matrix: failed to remove {} during corruption recovery: {e}. Fix permissions or wipe the directory manually.",
                store.display()
            ));
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

        // The saved device_id is canonical — it's what the server actually
        // assigned at login. config.device_id is only a hint for first-ever
        // login. If they drift (e.g. after auto-recovery generates a fresh
        // device, or the operator edits config), warn but honor the saved
        // value. Wiping on drift would create a recovery loop.
        if let (Some(blob), Some(want)) = (
            saved.as_ref(),
            config.device_id.as_deref().filter(|s| !s.is_empty()),
        ) && want != blob.device_id
        {
            warn!(
                "matrix: configured channels.matrix.device-id ({want}) differs from the saved session ({}). \
                 Honoring the saved device_id (canonical, assigned by the homeserver). \
                 Update channels.matrix.device-id to match (or clear it) to silence this warning, \
                 or wipe {} entirely to register a different device.",
                blob.device_id,
                state_dir.display(),
            );
        }

        // Detect orphan crypto state — store data without a session blob.
        // This typically happens after a manual `rm session.json` or when a
        // prior install crashed mid-write. Restoring is impossible; logging
        // in fresh on top of the orphan store reproduces the same
        // SigningKeyChanged / Duplicate-OTK loop the user just hit.
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
            .build()
            .await
            .context("build matrix client")?;

        // Step 1: restore an existing session, or fresh-login.
        let mut fresh_login = false;
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
                Ok(()) => info!("matrix: restored session from session.json"),
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

            // Durable corruption signal: when the matrix-sdk encounters a
            // duplicate-OTK upload (the server says it already has the
            // one-time-keys we're trying to upload), it persists this flag
            // (encryption/mod.rs:715-720). Per the SDK comment at line 691,
            // this means "we forgot about some of our one-time keys. This
            // will lead to UTDs." It survives restarts. The only way out is
            // to wipe and re-login.
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
            fresh_login = true;
            if let Some(blob) = session_blob_from(&client)
                && let Err(e) = session::save(state_dir, &blob)
            {
                warn!("matrix: failed to persist session.json: {e}");
            }
        }

        // Step 2: import existing cross-signing + room keys from the
        // homeserver's encrypted backup. Failure here (wrong recovery_key,
        // missing backup, secret-storage rotated) is non-fatal — bootstrap
        // below fills in fresh cross-signing instead. The operator should
        // see the warning and either fix the recovery key or accept fresh
        // bootstrap as the new baseline.
        if let Some(key) = config.recovery_key.as_deref()
            && !key.is_empty()
        {
            run_recovery(&client, key).await;
        }

        // Step 3: ensure cross-signing is set up. Idempotent — a no-op when
        // recover() imported an existing identity, fresh bootstrap otherwise.
        if fresh_login
            && let Some(pw) = config.password.as_deref().filter(|s| !s.is_empty())
            && let Some(user_id) = client.user_id()
        {
            ensure_cross_signing(&client, user_id.as_str().to_string(), pw.to_string()).await;
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
            warn!(
                "matrix: {reason} Auto-recovering: wiping {} and re-authenticating with password.",
                state_dir.display()
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
        if !config.access_token.is_empty() {
            return access_token_login(client, config).await;
        }
        bail!("matrix login requires either access_token or user_id+password")
    }

    async fn password_login(client: &Client, config: &MatrixConfig, password: &str) -> Result<()> {
        let user_id = config
            .user_id
            .clone()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("matrix.user_id is required for password login"))?;
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
        info!("matrix: logged in via password");
        Ok(())
    }

    async fn access_token_login(client: &Client, config: &MatrixConfig) -> Result<()> {
        let user_id = config
            .user_id
            .clone()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!("matrix.user_id is required when using access_token-based login")
            })?
            .parse()
            .context("parse matrix.user_id")?;
        let device_id = config
            .device_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("ZEROCLAW_{}", uuid::Uuid::new_v4().simple()));
        let session = MatrixSession {
            meta: SessionMeta {
                user_id,
                device_id: device_id.into(),
            },
            tokens: SessionTokens {
                access_token: config.access_token.clone(),
                refresh_token: None,
            },
        };
        client
            .matrix_auth()
            .restore_session(session, matrix_sdk::store::RoomLoadSettings::default())
            .await
            .context("attach matrix session via access_token")?;
        info!("matrix: logged in via access_token");
        Ok(())
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

    /// Try to import cross-signing keys + room keys from the homeserver's
    /// encrypted backup, decrypted by `key`. Logs success or failure; failure
    /// is non-fatal (caller will fall through to `ensure_cross_signing`,
    /// which bootstraps fresh keys if no identity was imported).
    async fn run_recovery(client: &Client, key: &str) {
        let recovery = client.encryption().recovery();
        if matches!(
            recovery.state(),
            matrix_sdk::encryption::recovery::RecoveryState::Enabled
        ) {
            debug!("matrix: recovery already enabled, skipping recover()");
            return;
        }
        match recovery.recover(key).await {
            Ok(()) => {
                info!("matrix: E2EE recovery completed (cross-signing + room keys imported)")
            }
            Err(e) => warn!(
                "matrix: E2EE recovery failed: {e} — ensure channels.matrix.recovery-key matches the homeserver's secret-storage key. Continuing with fresh bootstrap."
            ),
        }
    }

    /// Ensure cross-signing is set up for this device. After a successful
    /// `recover()`, the user identity already exists and this is a no-op.
    /// On a brand-new account with no server-side backup, this bootstraps
    /// fresh cross-signing keys (UIA-protected by `password`).
    ///
    /// We deliberately do *not* call `recovery().enable()` here: it would
    /// generate a new secret-storage key and invalidate the user's
    /// configured `recovery_key`.
    async fn ensure_cross_signing(client: &Client, user_id: String, password: String) {
        let identifier = UserIdentifier::UserIdOrLocalpart(user_id);
        let auth_data = AuthData::Password(Password::new(identifier, password));
        match client
            .encryption()
            .bootstrap_cross_signing_if_needed(Some(auth_data))
            .await
        {
            Ok(()) => info!("matrix: cross-signing verified or bootstrapped"),
            Err(e) => warn!("matrix: cross-signing setup failed: {e}"),
        }
    }

    pub(super) async fn resolve_room(
        client: &Client,
        cache: &Arc<RwLock<HashMap<String, OwnedRoomId>>>,
        id_or_alias: &str,
    ) -> Result<OwnedRoomId> {
        // Be lenient with `<anything>||<room-id-or-alias>` recipients (some
        // operators write cron `delivery.to` that way). Extract the last
        // segment that parses as a Matrix room id (`!…`) or alias (`#…`).
        let id_or_alias = if id_or_alias.contains("||") {
            let segments: Vec<&str> = id_or_alias.split("||").map(|s| s.trim()).collect();
            let chosen = segments
                .iter()
                .rev()
                .find(|s| s.starts_with('!') || s.starts_with('#'))
                .copied()
                .unwrap_or(id_or_alias);
            warn!(
                "matrix: recipient {id_or_alias:?} contains `||`; using {chosen:?} as the room target. Update channels.matrix or cron `delivery.to` to a plain room id/alias to silence this warning."
            );
            chosen
        } else {
            id_or_alias
        };
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
        time::SystemTime,
    };

    use matrix_sdk::{
        Client, Room, RoomState,
        config::SyncSettings,
        event_handler::RawEvent,
        ruma::{
            OwnedEventId, OwnedUserId,
            events::{
                AnySyncTimelineEvent,
                room::message::{MessageType, OriginalSyncRoomMessageEvent},
            },
            serde::Raw,
        },
    };
    use serde_json::Value as JsonValue;
    use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock, mpsc, oneshot};
    use tracing::{debug, error, info, warn};

    use super::{allowlist, approval, context as ctx_mod, mention};
    use crate::transcription::TranscriptionManager;
    use zeroclaw_api::{
        channel::{ChannelApprovalResponse, ChannelMessage},
        media::MediaAttachment,
    };
    use zeroclaw_config::schema::{MatrixConfig, TranscriptionConfig};

    #[derive(Clone)]
    pub(super) struct HandlerCtx {
        pub config: Arc<MatrixConfig>,
        pub transcription: Option<Arc<TranscriptionConfig>>,
        pub workspace_dir: Option<Arc<std::path::PathBuf>>,
        pub tx: mpsc::Sender<ChannelMessage>,
        pub pending_approvals:
            Arc<TokioMutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>,
        pub threads_seen: Arc<TokioRwLock<HashSet<OwnedEventId>>>,
        pub bot_user_id: OwnedUserId,
        pub bot_display_name: Arc<TokioRwLock<Option<String>>>,
        pub initial_sync_done: Arc<AtomicBool>,
    }

    pub(super) async fn run_sync_loop(client: Client, ctx: HandlerCtx) -> anyhow::Result<()> {
        let handler_ctx = ctx.clone();
        client.add_event_handler(
            move |ev: OriginalSyncRoomMessageEvent, room: Room, raw: RawEvent| {
                let ctx = handler_ctx.clone();
                async move {
                    if let Err(e) = handle_message(ctx, ev, room, raw).await {
                        warn!("matrix: handle_message failed: {e}");
                    }
                }
            },
        );

        info!("matrix: starting sync loop");
        // Run an initial sync once so the sync token + state are populated,
        // then flip the health flag and enter the long-running sync loop.
        if let Err(e) = client.sync_once(SyncSettings::default()).await {
            return Err(anyhow::anyhow!("matrix initial sync failed: {e}"));
        }
        ctx.initial_sync_done.store(true, Ordering::SeqCst);
        client
            .sync(SyncSettings::default())
            .await
            .map_err(|e| anyhow::anyhow!("matrix sync loop failed: {e}"))
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

        if !allowlist::user_allowed(&ctx.config.allowed_users, sender) {
            debug!("matrix: drop message from non-allowed sender {sender}");
            return Ok(());
        }
        if !allowlist::room_allowed_static(&ctx.config.allowed_rooms, room_id) {
            debug!("matrix: drop message from non-allowed room {room_id}");
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
                debug!("matrix: drop unmentioned message from {sender}");
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
                Err(e) => warn!("matrix: failed to fetch thread root {tid}: {e}"),
            }
        }

        // Process inbound media: download, persist to {workspace}/matrix_files/,
        // and emit a content marker the runtime's vision/document pipeline reads.
        // The runtime ignores `ChannelMessage.attachments` for vision — markers
        // in `content` are how Telegram and the multimodal pipeline communicate
        // (see telegram.rs `format_attachment_content`). We always leave
        // `attachments` empty.
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
                Some(MediaInfo::new(
                    m.source.clone(),
                    m.body.clone(),
                    m.info.as_ref().and_then(|i| i.mimetype.clone()),
                    kind,
                ))
            }
            _ => None,
        };

        if let Some(info) = media_kind {
            match save_media_to_workspace(&room, &info, ctx.workspace_dir.as_deref()).await {
                Ok(Some(path)) => {
                    let marker = format_media_marker(&info, &path);
                    // Replace the placeholder body ("[image]"/"[file]"/etc.)
                    // emitted by ctx_mod::body_for with the real marker. If the
                    // user sent a caption, body would be the caption — append.
                    let placeholder =
                        matches!(body.as_str(), "[image]" | "[file]" | "[audio]" | "[video]");
                    content = if placeholder || body.is_empty() || body == info.file_name {
                        marker
                    } else {
                        format!("{content}\n\n{marker}")
                    };

                    // Voice message transcription happens AFTER save so the
                    // agent gets both the transcript text and a path to the
                    // raw audio file.
                    if matches!(info.kind, MediaCategory::Voice)
                        && let Some(transcription) = ctx.transcription.as_ref()
                        && transcription.enabled
                    {
                        match transcribe_from_disk(transcription, &path, &info.file_name).await {
                            Ok(text) if !text.trim().is_empty() => {
                                content = format!("[voice transcript]: {text}\n\n{content}");
                            }
                            Ok(_) => {}
                            Err(e) => warn!("matrix: voice transcription failed: {e}"),
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => warn!("matrix: media handling failed: {e}"),
            }
        }
        let attachments: Vec<MediaAttachment> = Vec::new();

        let msg = ChannelMessage {
            id: ev.event_id.to_string(),
            sender: sender.to_string(),
            reply_target: room.room_id().to_string(),
            content,
            channel: "matrix".to_string(),
            timestamp: SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            // Reply anchor: use the existing thread root when present,
            // otherwise (when reply_in_thread is on) anchor a brand-new thread
            // on this very event so the bot's reply opens a thread.
            thread_ts: thread_id.as_ref().map(|t| t.to_string()).or_else(|| {
                if ctx.config.reply_in_thread {
                    Some(ev.event_id.to_string())
                } else {
                    None
                }
            }),
            // Interruption scope is for cancellation grouping — only set when
            // the inbound is genuinely *inside* a reply thread.
            interruption_scope_id: thread_id.as_ref().map(|t| t.to_string()),
            attachments,
        };

        if let Err(e) = ctx.tx.send(msg).await {
            error!("matrix: failed to forward inbound message: {e}");
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
            warn!(
                "matrix: cannot persist {} — channels.matrix workspace_dir not configured. Set ZEROCLAW_DIR or run via the orchestrator.",
                info.file_name
            );
            return Ok(None);
        };
        let dir = workspace.join("matrix_files");
        std::fs::create_dir_all(&dir)
            .map_err(|e| anyhow::anyhow!("create {}: {e}", dir.display()))?;
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
            .map_err(|e| anyhow::anyhow!("get_media_content ({source_kind}): {e}"))?;

        let safe_name = sanitize_filename(&info.file_name, &info.kind, info.mime.as_deref());
        // Disambiguate by uuid prefix to avoid collisions across messages.
        let unique = format!("{}_{safe_name}", uuid::Uuid::new_v4().simple());
        let path = dir.join(unique);
        std::fs::write(&path, &bytes)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))?;
        info!(
            "matrix: saved {} bytes ({}) to {}",
            bytes.len(),
            source_kind,
            path.display()
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
        let bytes =
            std::fs::read(path).map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        let manager = TranscriptionManager::new(config)?;
        manager.transcribe(&bytes, file_name).await
    }
}

// ─── outbound ──────────────────────────────────────────────────────────────
mod outbound {
    use std::{collections::HashMap, sync::Arc};

    use anyhow::{Context as _, Result, anyhow, bail};
    use matrix_sdk::{
        Client, Room, RoomState,
        attachment::AttachmentConfig,
        room::{
            edit::EditedContent,
            reply::{EnforceThread, Reply},
        },
        ruma::{
            OwnedEventId, OwnedRoomId,
            events::{
                reaction::ReactionEventContent,
                relation::Annotation,
                room::message::{
                    MessageType, ReplyWithinThread, RoomMessageEventContent,
                    RoomMessageEventContentWithoutRelation, TextMessageEventContent,
                },
            },
        },
    };
    use serde_json::json;
    use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock};
    use tracing::warn;

    use super::{client, context as ctx_mod, markers};
    use zeroclaw_api::{channel::SendMessage, media::MediaAttachment};

    pub(super) type ReactionKey = (OwnedRoomId, OwnedEventId, String);

    pub(super) struct Outbox<'a> {
        pub client: &'a Client,
        pub alias_cache: &'a Arc<TokioRwLock<HashMap<String, OwnedRoomId>>>,
        pub threads_seen: &'a Arc<TokioRwLock<std::collections::HashSet<OwnedEventId>>>,
        pub reaction_log: &'a Arc<TokioMutex<HashMap<ReactionKey, OwnedEventId>>>,
        pub reply_in_thread: bool,
    }

    pub(super) async fn send(outbox: &Outbox<'_>, message: &SendMessage) -> Result<OwnedEventId> {
        let room =
            resolve_joined_room(outbox.client, outbox.alias_cache, &message.recipient).await?;

        let (mut text, ms) = markers::parse(&message.content);

        // Outbound attachments. SendMessage.attachments comes from the runtime's
        // structured attachment list; missing/empty data is fatal there because
        // the bytes were already in memory. Marker-driven uploads are best-
        // effort: if a marker target can't be read or uploaded, log it and fall
        // back to a textual note so the operator sees what the agent intended
        // rather than a silently-dropped reply.
        for att in &message.attachments {
            upload_attachment(&room, att, AttachmentKind::Auto).await?;
        }

        let mut failed_markers: Vec<String> = Vec::new();
        for marker in &ms {
            let kind = match marker.kind {
                markers::MarkerKind::Image => AttachmentKind::Image,
                markers::MarkerKind::Audio => AttachmentKind::Audio,
                markers::MarkerKind::Video => AttachmentKind::Video,
                markers::MarkerKind::File => AttachmentKind::File,
                markers::MarkerKind::Voice => AttachmentKind::Voice,
            };
            let bytes = match fetch_marker_bytes(&marker.target).await {
                Ok(b) => b,
                Err(e) => {
                    warn!(
                        "matrix: skipping outbound marker for {}: {e}",
                        marker.target
                    );
                    failed_markers.push(marker.target.clone());
                    continue;
                }
            };
            let file_name = derive_file_name(&marker.target);
            let mime = mime_for(&file_name, &kind);
            let att = MediaAttachment {
                file_name,
                data: bytes,
                mime_type: Some(mime),
            };
            if let Err(e) = upload_attachment(&room, &att, kind).await {
                warn!(
                    "matrix: skipping outbound marker for {} (upload failed): {e}",
                    marker.target
                );
                failed_markers.push(marker.target.clone());
            }
        }

        if !failed_markers.is_empty() {
            let note = if failed_markers.len() == 1 {
                format!(
                    "(note: I couldn't deliver the file at {}.)",
                    failed_markers[0]
                )
            } else {
                let joined = failed_markers.join(", ");
                format!("(note: I couldn't deliver these files: {joined}.)")
            };
            text = if text.trim().is_empty() {
                note
            } else {
                format!("{text}\n\n{note}")
            };
        }

        if text.trim().is_empty() {
            return Err(anyhow!(
                "matrix: empty message body after stripping markers"
            ));
        }

        let content = RoomMessageEventContent::text_markdown(&text);

        let event_id = if let (true, Some(anchor)) = (
            outbox.reply_in_thread,
            message.thread_ts.as_deref().filter(|s| !s.is_empty()),
        ) {
            send_threaded_reply(&room, content, anchor, outbox.threads_seen).await?
        } else {
            room.send(content).await?.event_id
        };

        Ok(event_id)
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
                },
            )
            .await
            .map_err(|e| anyhow!("make_reply_event failed: {e}"))?;
        ctx_mod::mark_seen(threads_seen, anchor).await;
        let resp = room.send(reply_event).await?;
        Ok(resp.event_id)
    }

    pub(super) async fn edit(
        client: &Client,
        room_id: &str,
        event_id: &OwnedEventId,
        text: &str,
    ) -> Result<()> {
        let room = client
            .get_room(&room_id.parse::<OwnedRoomId>()?)
            .ok_or_else(|| anyhow!("matrix: room not joined: {room_id}"))?;
        let new_content = RoomMessageEventContentWithoutRelation::new(MessageType::Text(
            TextMessageEventContent::markdown(text),
        ));
        let edit_event = room
            .make_edit_event(event_id, EditedContent::RoomMessage(new_content))
            .await
            .map_err(|e| anyhow!("make_edit_event failed: {e}"))?;
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
            .ok_or_else(|| anyhow!("matrix: room not joined: {room_id}"))?;
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
            resp.event_id,
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

    async fn resolve_joined_room(
        client: &Client,
        cache: &Arc<TokioRwLock<HashMap<String, OwnedRoomId>>>,
        recipient: &str,
    ) -> Result<Room> {
        let id = client::resolve_room(client, cache, recipient).await?;
        let room = client
            .get_room(&id)
            .ok_or_else(|| anyhow!("matrix: bot is not in room {recipient}"))?;
        if room.state() != RoomState::Joined {
            bail!("matrix: room {recipient} is not in joined state");
        }
        Ok(room)
    }

    enum AttachmentKind {
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
    ) -> Result<()> {
        let mime: mime_guess::Mime = match att.mime_type.as_deref() {
            Some(m) => m
                .parse()
                .unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM),
            None => mime_guess::from_path(&att.file_name)
                .first()
                .unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM),
        };
        if matches!(kind, AttachmentKind::Voice) {
            return upload_voice(room, att, &mime).await;
        }
        room.send_attachment(
            att.file_name.clone(),
            &mime,
            att.data.clone(),
            AttachmentConfig::new(),
        )
        .await
        .map_err(|e| anyhow!("send_attachment failed: {e}"))?;
        Ok(())
    }

    /// Voice messages need the `org.matrix.msc3245.voice` flag, which the
    /// stable matrix-sdk types don't carry. Send via raw JSON.
    async fn upload_voice(
        room: &Room,
        att: &MediaAttachment,
        mime: &mime_guess::Mime,
    ) -> Result<()> {
        let mxc = room
            .client()
            .media()
            .upload(mime, att.data.clone(), None)
            .await
            .map_err(|e| anyhow!("media upload failed: {e}"))?;
        let event = json!({
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
        room.send_raw("m.room.message", event).await?;
        Ok(())
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

    async fn fetch_marker_bytes(target: &str) -> Result<Vec<u8>> {
        if target.starts_with("http://") || target.starts_with("https://") {
            let resp = reqwest::get(target)
                .await
                .with_context(|| format!("fetch marker target {target}"))?;
            let bytes = resp
                .bytes()
                .await
                .with_context(|| format!("read marker bytes from {target}"))?;
            return Ok(bytes.to_vec());
        }
        let bytes = std::fs::read(target).with_context(|| format!("read marker file {target}"))?;
        Ok(bytes)
    }
}

// ─── public type ───────────────────────────────────────────────────────────

/// Matrix channel.
pub struct MatrixChannel {
    config: Arc<MatrixConfig>,
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
}

impl MatrixChannel {
    /// Validate config and prepare the channel. The SDK Client is built lazily
    /// on first `listen()` or `send()` call.
    pub fn new(config: MatrixConfig, state_dir: PathBuf) -> Result<Self> {
        if config.homeserver.trim().is_empty() {
            bail!("matrix: `homeserver` is required");
        }
        if config.access_token.trim().is_empty() && config.password.is_none() {
            bail!("matrix: configure either `access_token` or `password`");
        }
        Ok(Self {
            config: Arc::new(config),
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
        })
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
        self.client
            .get_or_try_init(|| async {
                let c = client::build(&self.config, &self.state_dir).await?;
                if let Ok(Some(name)) = c.account().get_display_name().await {
                    *self.bot_display_name.write().await = Some(name);
                }
                Ok::<_, anyhow::Error>(c)
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
        }
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &str {
        "matrix"
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
            .ok_or_else(|| anyhow!("matrix: client has no user_id after login"))?
            .to_owned();
        let ctx = inbound::HandlerCtx {
            config: self.config.clone(),
            transcription: self.transcription.clone(),
            workspace_dir: self.workspace_dir.clone(),
            tx,
            pending_approvals: self.pending_approvals.clone(),
            threads_seen: self.threads_seen.clone(),
            bot_user_id: user_id,
            bot_display_name: self.bot_display_name.clone(),
            initial_sync_done: self.initial_sync_done.clone(),
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
        matches!(self.config.stream_mode, StreamMode::Partial)
    }

    fn supports_multi_message_streaming(&self) -> bool {
        matches!(self.config.stream_mode, StreamMode::MultiMessage)
    }

    fn multi_message_delay_ms(&self) -> u64 {
        self.config.multi_message_delay_ms
    }

    async fn send_draft(&self, message: &SendMessage) -> Result<Option<String>> {
        let client = self.ensure_client().await?;
        let event_id = outbound::send(&self.outbox(client), message).await?;
        let key = streaming_key(&message.recipient, message.thread_ts.as_deref())?;
        let mut state = self.streaming_state.write().await;
        match self.config.stream_mode {
            StreamMode::Partial | StreamMode::Off => {
                state.partial.insert(
                    key,
                    streaming::PartialDraft {
                        event_id: event_id.clone(),
                        last_text: message.content.clone(),
                        last_edit: Instant::now(),
                    },
                );
            }
            StreamMode::MultiMessage => {
                state.multi.insert(
                    key,
                    streaming::MultiDraft {
                        current_event_id: event_id.clone(),
                    },
                );
            }
        }
        Ok(Some(event_id.to_string()))
    }

    async fn update_draft(&self, recipient: &str, _message_id: &str, text: &str) -> Result<()> {
        let client = self.ensure_client().await?;
        let key = streaming_key(recipient, None)?;
        let event_id = {
            let mut state = self.streaming_state.write().await;
            let Some(draft) = state.partial.get_mut(&key) else {
                return Ok(());
            };
            let now = Instant::now();
            let interval = Duration::from_millis(self.config.draft_update_interval_ms.max(50));
            if !streaming::partial_should_edit(draft, text, now, interval) {
                return Ok(());
            }
            let event_id = draft.event_id.clone();
            draft.last_text = text.to_string();
            draft.last_edit = now;
            event_id
        };
        outbound::edit(client, recipient, &event_id, text).await
    }

    async fn update_draft_progress(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> Result<()> {
        self.update_draft(recipient, message_id, text).await
    }

    async fn finalize_draft(&self, recipient: &str, _message_id: &str, text: &str) -> Result<()> {
        let client = self.ensure_client().await?;
        let key = streaming_key(recipient, None)?;
        let event_id = {
            let mut state = self.streaming_state.write().await;
            if let Some(d) = state.partial.remove(&key) {
                Some(d.event_id)
            } else {
                state.multi.remove(&key).map(|d| d.current_event_id)
            }
        };
        if let Some(eid) = event_id {
            outbound::edit(client, recipient, &eid, text).await?;
        }
        Ok(())
    }

    async fn cancel_draft(&self, recipient: &str, _message_id: &str) -> Result<()> {
        let client = self.ensure_client().await?;
        let key = streaming_key(recipient, None)?;
        let event_id = {
            let mut state = self.streaming_state.write().await;
            if let Some(d) = state.partial.remove(&key) {
                Some(d.event_id)
            } else {
                state.multi.remove(&key).map(|d| d.current_event_id)
            }
        };
        if let Some(eid) = event_id {
            let _ = outbound::redact(client, recipient, &eid, Some("cancelled".to_string())).await;
        }
        Ok(())
    }

    async fn add_reaction(&self, channel_id: &str, message_id: &str, emoji: &str) -> Result<()> {
        if !self.config.ack_reactions {
            return Ok(());
        }
        let client = self.ensure_client().await?;
        let event_id: OwnedEventId = message_id.parse()?;
        outbound::react(&self.outbox(client), channel_id, &event_id, emoji).await
    }

    async fn remove_reaction(&self, channel_id: &str, message_id: &str, emoji: &str) -> Result<()> {
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
        let send_msg = SendMessage::new(prompt, recipient);
        self.send(&send_msg).await?;

        let (tx, rx) = oneshot::channel();
        self.pending_approvals
            .lock()
            .await
            .insert(token.clone(), tx);
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

fn streaming_key(recipient: &str, thread: Option<&str>) -> Result<streaming::DraftKey> {
    let room: OwnedRoomId = recipient
        .parse()
        .with_context(|| format!("parse recipient room id {recipient}"))?;
    let thread = match thread {
        Some(t) if !t.is_empty() => Some(
            t.parse::<OwnedEventId>()
                .with_context(|| format!("parse thread anchor {t}"))?,
        ),
        _ => None,
    };
    Ok((room, thread))
}

// ─── tests ─────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
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
        use super::super::streaming::{PartialDraft, partial_should_edit};
        use matrix_sdk::ruma::owned_event_id;
        use std::time::{Duration, Instant};

        fn draft(text: &str, last_edit: Instant) -> PartialDraft {
            PartialDraft {
                event_id: owned_event_id!("$1:server"),
                last_text: text.to_string(),
                last_edit,
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
        fn corrupt_returns_err() {
            let dir = TempDir::new().unwrap();
            let p = dir.path().join("session.json");
            std::fs::write(p, "{not valid json").unwrap();
            assert!(load(dir.path()).is_err());
        }
    }

    mod auth_gating {
        //! Pure-logic tests for the auth-flow gating helpers — keeps
        //! corruption-recovery decisions verifiable without touching the SDK.

        use super::super::client::{can_password_relogin, store_has_orphan_data};
        use tempfile::TempDir;
        use zeroclaw_config::schema::MatrixConfig;

        fn cfg(password: Option<&str>, user_id: Option<&str>) -> MatrixConfig {
            MatrixConfig {
                enabled: true,
                homeserver: "https://m.org".into(),
                access_token: String::new(),
                user_id: user_id.map(String::from),
                device_id: None,
                allowed_users: vec![],
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
                ack_reactions: true,
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
    }

    mod cross_signing {
        //! Cross-signing bootstrap is gated on (password.is_some()
        //! && recovery_key.is_some() && fresh_login). This test asserts the
        //! gating predicate, not the SDK call itself.

        fn should_bootstrap(
            password: Option<&str>,
            recovery_key: Option<&str>,
            fresh_login: bool,
        ) -> bool {
            fresh_login
                && matches!(password, Some(p) if !p.is_empty())
                && matches!(recovery_key, Some(r) if !r.is_empty())
        }

        #[test]
        fn both_set_fresh_login() {
            assert!(should_bootstrap(Some("pw"), Some("EsTk"), true));
        }

        #[test]
        fn password_only() {
            assert!(!should_bootstrap(Some("pw"), None, true));
        }

        #[test]
        fn recovery_only() {
            assert!(!should_bootstrap(None, Some("EsTk"), true));
        }

        #[test]
        fn restored_session_skips_bootstrap() {
            assert!(!should_bootstrap(Some("pw"), Some("EsTk"), false));
        }

        #[test]
        fn empty_strings_dont_count() {
            assert!(!should_bootstrap(Some(""), Some("EsTk"), true));
            assert!(!should_bootstrap(Some("pw"), Some(""), true));
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
        use super::super::inbound::{extract_mentions_user_ids, extract_thread_id};
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
}
