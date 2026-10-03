//! JSONL-based session persistence for channel conversations.

use crate::session_backend::{
    ScopedSessionAccess, SessionBackend, SessionContext, SessionMetadata, check_session_ownership,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use zeroclaw_api::model_provider::ChatMessage;
pub use zeroclaw_api::session_keys::sanitize_session_key;
use zeroclaw_api::session_keys::{
    JSONL_SESSION_FILE_SUFFIX as SESSION_FILE_SUFFIX,
    JSONL_SESSION_METADATA_FILE_SUFFIX as METADATA_FILE_SUFFIX,
    JSONL_SESSION_MIGRATED_METADATA_FILE_SUFFIX,
};

#[derive(Default)]
pub(crate) struct MutationState {
    migrated: bool,
    receipt_state_uncertain: bool,
}

/// Durable facts that cannot be recovered from the append-only message file.
/// Message count and timestamps remain derived from the session files at read
/// time so the JSONL backend does not duplicate them in a second source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JsonlSessionMetadata {
    pub(crate) name: Option<String>,
    pub(crate) agent_alias: Option<String>,
    pub(crate) channel_id: Option<String>,
    pub(crate) room_id: Option<String>,
    pub(crate) sender_id: Option<String>,
}

fn metadata_sidecar_path(sessions_dir: &Path, session_key: &str) -> PathBuf {
    sessions_dir.join(format!(
        "{}{METADATA_FILE_SUFFIX}",
        sanitize_session_key(session_key)
    ))
}

/// Read a session's ownership sidecar without taking the per-directory
/// mutation lock. SQLite migration already holds that lock for the whole
/// handoff, so it must not re-enter it through `SessionStore`.
pub(crate) fn read_metadata_sidecar(
    sessions_dir: &Path,
    session_key: &str,
) -> std::io::Result<Option<JsonlSessionMetadata>> {
    match std::fs::File::open(metadata_sidecar_path(sessions_dir, session_key)) {
        Ok(file) => serde_json::from_reader(std::io::BufReader::new(file))
            .map(Some)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Archive a session's ownership sidecar alongside its migrated JSONL file.
///
/// Callers reach this only after the transcript is archived and the sidecar's
/// ownership fields are committed to SQLite, so a sidecar left live here would
/// list as an empty session that no longer has a transcript. When the rename
/// cannot complete, delete the sidecar instead and surface the rename failure
/// only if that cleanup also fails.
pub(crate) fn mark_metadata_sidecar_migrated(
    sessions_dir: &Path,
    session_key: &str,
) -> std::io::Result<()> {
    let path = metadata_sidecar_path(sessions_dir, session_key);
    if !path.exists() {
        return Ok(());
    }
    let migrated = sessions_dir.join(format!(
        "{}{}",
        sanitize_session_key(session_key),
        JSONL_SESSION_MIGRATED_METADATA_FILE_SUFFIX
    ));
    let Err(rename_error) = std::fs::rename(&path, migrated) else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(remove_error) if remove_error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(rename_error),
    }
}

pub(crate) type MutationLock = parking_lot::Mutex<MutationState>;

struct MutationLockRecord {
    lock: Weak<MutationLock>,
    migrated: bool,
}

static MUTATION_LOCKS: OnceLock<parking_lot::Mutex<HashMap<PathBuf, MutationLockRecord>>> =
    OnceLock::new();

/// Append-only JSONL session store for channel conversations.
pub struct SessionStore {
    sessions_dir: PathBuf,
    mutation_lock: Arc<MutationLock>,
}

impl SessionStore {
    /// Create a new session store, ensuring the sessions directory exists.
    pub fn new(workspace_dir: &Path) -> std::io::Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        let mutation_lock = mutation_lock_for(&sessions_dir)?;
        {
            let mut state = mutation_lock.lock();
            match crate::session_sqlite::has_committed_jsonl_import_receipts(workspace_dir) {
                Ok(true) => mark_session_directory_migrated(&sessions_dir, &mut state)?,
                Ok(false) => state.receipt_state_uncertain = false,
                Err(error) => {
                    state.receipt_state_uncertain = true;
                    return Err(std::io::Error::other(format!(
                        "Failed to inspect durable JSONL migration state: {error:#}"
                    )));
                }
            }
        }
        Ok(Self {
            sessions_dir,
            mutation_lock,
        })
    }

    /// Compute the file path for a session key, sanitizing for filesystem safety.
    fn session_path(&self, session_key: &str) -> PathBuf {
        self.sessions_dir.join(format!(
            "{}{SESSION_FILE_SUFFIX}",
            sanitize_session_key(session_key)
        ))
    }

    fn metadata_path(&self, session_key: &str) -> PathBuf {
        self.sessions_dir.join(format!(
            "{}{METADATA_FILE_SUFFIX}",
            sanitize_session_key(session_key)
        ))
    }

    fn read_metadata(&self, session_key: &str) -> std::io::Result<Option<JsonlSessionMetadata>> {
        let path = self.metadata_path(session_key);
        match std::fs::File::open(path) {
            Ok(file) => serde_json::from_reader(std::io::BufReader::new(file))
                .map(Some)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn write_metadata_unlocked(
        &self,
        session_key: &str,
        metadata: &JsonlSessionMetadata,
    ) -> std::io::Result<()> {
        let path = self.metadata_path(session_key);
        let mut temp = tempfile::NamedTempFile::new_in(&self.sessions_dir)?;
        serde_json::to_writer(&mut temp, metadata)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(path).map(|_| ()).map_err(|error| error.error)
    }

    fn update_metadata<F>(&self, session_key: &str, update: F) -> std::io::Result<()>
    where
        F: FnOnce(&mut JsonlSessionMetadata),
    {
        let _guard = self.mutation_guard()?;
        self.update_metadata_unlocked(session_key, update)
    }

    fn update_metadata_unlocked<F>(&self, session_key: &str, update: F) -> std::io::Result<()>
    where
        F: FnOnce(&mut JsonlSessionMetadata),
    {
        let mut metadata = self.read_metadata(session_key)?.unwrap_or_default();
        update(&mut metadata);
        // A sidecar is a claim on a real zero-or-more-message session, not a
        // standalone session representation. Materialize the empty transcript
        // first so hygiene, migration, and later appends always move the
        // ownership fact with its canonical message file.
        let transcript = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.session_path(session_key))?;
        transcript.sync_all()?;
        self.write_metadata_unlocked(session_key, &metadata)
    }

    fn metadata_for_session(&self, session_key: &str) -> Option<SessionMetadata> {
        let session_path = self.session_path(session_key);
        let metadata_path = self.metadata_path(session_key);
        if !session_path.exists() && !metadata_path.exists() {
            return None;
        }

        // Corrupt or unreadable sidecars deliberately degrade to unattributed
        // metadata. Scoped callers therefore fail closed without hiding the
        // underlying conversation from unscoped administrative surfaces.
        let persisted = self
            .read_metadata(session_key)
            .ok()
            .flatten()
            .unwrap_or_default();
        let message_count = self.load(session_key).len();
        let file_metadata = std::fs::metadata(&session_path)
            .or_else(|_| std::fs::metadata(&metadata_path))
            .ok();
        let last_activity = file_metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(Utc::now);
        let created_at = file_metadata
            .as_ref()
            .and_then(|metadata| metadata.created().ok())
            .map(DateTime::<Utc>::from)
            .unwrap_or(last_activity);

        Some(SessionMetadata {
            key: sanitize_session_key(session_key),
            name: persisted.name,
            created_at,
            last_activity,
            message_count,
            agent_alias: persisted.agent_alias,
            channel_id: persisted.channel_id,
            room_id: persisted.room_id,
            sender_id: persisted.sender_id,
            principal_id: None,
        })
    }

    /// Sidecar file recording whether `session_key`'s transcript currently
    /// starts with the synthetic trim breadcrumb. Kept as a canonical fact
    /// next to the transcript so restore never has to infer provenance from
    /// message text.
    fn trim_breadcrumb_path(&self, session_key: &str) -> PathBuf {
        self.sessions_dir.join(format!(
            "{}.trim_breadcrumb",
            sanitize_session_key(session_key)
        ))
    }

    /// Persist whether the session's transcript starts with the synthetic
    /// trim breadcrumb. Respects the migration fence: fails if the JSONL
    /// store has been migrated to SQLite.
    pub fn set_trim_breadcrumb(&self, session_key: &str, present: bool) -> std::io::Result<()> {
        let _guard = self.mutation_guard()?;
        std::fs::write(
            self.trim_breadcrumb_path(session_key),
            if present { b"1" as &[u8] } else { b"0" },
        )
    }

    /// Read the persisted breadcrumb flag. `None` if never recorded.
    ///
    /// Only an exact one-byte `b"0"` or `b"1"` is a verified reading; any
    /// other content (empty, truncated, extra bytes, a stray byte) is not
    /// something `set_trim_breadcrumb` ever wrote, so it is corruption, not a
    /// legitimate `false`. Treating it as `Some(false)` would let a restore
    /// mark an untrimmed transcript as trim-clean and skip persisting the
    /// correction. Callers must fail closed on this `Err`, the same as any
    /// other unreadable breadcrumb.
    pub fn get_trim_breadcrumb(&self, session_key: &str) -> std::io::Result<Option<bool>> {
        match std::fs::read(self.trim_breadcrumb_path(session_key)) {
            Ok(bytes) => match bytes.as_slice() {
                [b'0'] => Ok(Some(false)),
                [b'1'] => Ok(Some(true)),
                _ => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "corrupt trim breadcrumb for session {session_key}: expected a single \
                         b'0' or b'1' byte, got {} byte(s)",
                        bytes.len()
                    ),
                )),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Load all messages for a session from its JSONL file.
    /// Returns an empty vec if the path is not a regular JSONL session file or
    /// the file is unreadable.
    pub fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let path = self.session_path(session_key);
        match validate_jsonl_session_file_path(&path) {
            Ok(false) => return Vec::new(),
            Ok(true) => {}
            Err(_) => return Vec::new(),
        }
        let Ok(file) = std::fs::File::open(&path) else {
            return Vec::new();
        };
        let reader = std::io::BufReader::new(file);
        let mut messages = Vec::new();
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(msg) = serde_json::from_str::<ChatMessage>(trimmed) {
                messages.push(msg);
            }
        }
        messages
    }

    /// Like `load`, but fails closed: a missing file returns `Ok(empty)`,
    /// while an unreadable file, a line-read error, or a JSON parse failure
    /// returns `Err` so callers can distinguish "no session" from "existing
    /// transcript that could not be verified" instead of seeding a
    /// new-message-only cache over an existing transcript.
    pub fn try_load(&self, session_key: &str) -> std::io::Result<Vec<ChatMessage>> {
        let path = self.session_path(session_key);
        match validate_jsonl_session_file_path(&path) {
            Ok(false) => return Ok(Vec::new()),
            Ok(true) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        }
        let file = std::fs::File::open(&path)?;
        let reader = std::io::BufReader::new(file);
        let mut messages = Vec::new();
        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let msg: ChatMessage = serde_json::from_str(trimmed).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("corrupt session JSONL line: {e}"),
                )
            })?;
            messages.push(msg);
        }
        Ok(messages)
    }

    /// Append a single message to the session JSONL file.
    pub fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let _guard = self.mutation_guard()?;
        self.append_unlocked(session_key, message)
    }

    fn mutation_guard(&self) -> std::io::Result<parking_lot::MutexGuard<'_, MutationState>> {
        let guard = self.mutation_lock.lock();
        if guard.migrated || guard.receipt_state_uncertain {
            return Err(std::io::Error::other(
                "JSONL session store is inactive after SQLite migration",
            ));
        }
        Ok(guard)
    }

    fn append_unlocked(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let path = self.session_path(session_key);
        validate_jsonl_session_file_path(&path)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        let json = serde_json::to_string(message)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        writeln!(file, "{json}")?;
        Ok(())
    }

    /// Remove the last message from a session's JSONL file.
    /// Rewrite approach: load all messages, drop the last, rewrite. This is
    /// O(n) but rollbacks are rare.
    pub fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let _guard = self.mutation_guard()?;
        let mut messages = self.load(session_key);
        if messages.is_empty() {
            return Ok(false);
        }
        messages.pop();
        self.rewrite(session_key, &messages)?;
        Ok(true)
    }

    /// Replace the last message without exposing an intermediate truncated session.
    pub fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        self.update_last_with(session_key, message, |temp, path| {
            temp.persist(path).map(|_| ()).map_err(|error| error.error)
        })
    }

    fn update_last_with<F>(
        &self,
        session_key: &str,
        message: &ChatMessage,
        persist: F,
    ) -> std::io::Result<bool>
    where
        F: FnOnce(tempfile::NamedTempFile, &Path) -> std::io::Result<()>,
    {
        let _guard = self.mutation_guard()?;
        let mut messages = self.load(session_key);
        let Some(last) = messages.last_mut() else {
            return Ok(false);
        };
        *last = message.clone();
        self.rewrite_with(session_key, &messages, persist)?;
        Ok(true)
    }

    /// Compact a session file by rewriting only valid messages (removes corrupt lines).
    pub fn compact(&self, session_key: &str) -> std::io::Result<()> {
        let _guard = self.mutation_guard()?;
        validate_jsonl_session_file_path(&self.session_path(session_key))?;
        let messages = self.load(session_key);
        self.rewrite(session_key, &messages)
    }

    fn rewrite(&self, session_key: &str, messages: &[ChatMessage]) -> std::io::Result<()> {
        self.rewrite_with(session_key, messages, |temp, path| {
            temp.persist(path).map(|_| ()).map_err(|error| error.error)
        })
    }

    /// Transcript+sidecar replacement assuming the caller already holds the
    /// mutation guard. Split out so the existence-guarded
    /// `replace_conversation_state_if_exists` can probe-then-write under one
    /// guard acquisition (`parking_lot` mutexes are not reentrant, so the
    /// trait methods cannot call each other while holding it).
    fn replace_locked(
        &self,
        _guard: &parking_lot::MutexGuard<'_, MutationState>,
        session_key: &str,
        messages: &[ChatMessage],
        breadcrumb_present: bool,
    ) -> std::io::Result<()> {
        let previous_messages = self.load(session_key);
        self.rewrite(session_key, messages)?;
        let breadcrumb_write = std::fs::write(
            self.trim_breadcrumb_path(session_key),
            if breadcrumb_present {
                b"1" as &[u8]
            } else {
                b"0"
            },
        );
        if let Err(e) = breadcrumb_write {
            // Best-effort: if this rewrite also fails, the transcript is left
            // at the new value with the stale flag, and the caller must
            // reconcile by reloading both files rather than trusting either
            // write succeeded.
            let _ = self.rewrite(session_key, &previous_messages);
            return Err(e);
        }
        Ok(())
    }

    fn rewrite_with<F>(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
        persist: F,
    ) -> std::io::Result<()>
    where
        F: FnOnce(tempfile::NamedTempFile, &Path) -> std::io::Result<()>,
    {
        let path = self.session_path(session_key);
        let mut temp = tempfile::NamedTempFile::new_in(&self.sessions_dir)?;
        for msg in messages {
            serde_json::to_writer(&mut temp, msg)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            temp.write_all(b"\n")?;
        }

        temp.as_file().sync_all()?;
        persist(temp, &path)
    }

    /// Clear all messages from a session by truncating its JSONL file.
    /// The file is preserved (empty) so the session key remains in `list_sessions`.
    /// Also removes the breadcrumb sidecar: a reset session has no synthetic
    /// marker, so a stale recorded `true` must not survive into the next
    /// first message and be mistaken for a real trim. The sidecar is removed
    /// even when the transcript is absent or not a regular JSONL file: it is
    /// independently creatable by `set_trim_breadcrumb`, so a sidecar-only
    /// state (e.g. after a transcript write failure or a prior cleanup that
    /// removed the transcript but not its provenance file) must not survive
    /// a `clear_messages` call and be misread by a later restore.
    pub fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let _guard = self.mutation_guard()?;
        let _ = std::fs::remove_file(self.trim_breadcrumb_path(session_key));
        if !is_regular_jsonl_session_file(&self.session_path(session_key)) {
            return Ok(0);
        }
        let count = self.load(session_key).len();
        if count > 0 {
            self.rewrite(session_key, &[])?;
        }
        Ok(count)
    }

    /// Delete a session's JSONL file and its breadcrumb sidecar. Returns
    /// `true` if either file existed. The sidecar is removed even when the
    /// transcript file is already absent (or not a regular JSONL session
    /// file) so stale provenance cannot affect a later session that reuses
    /// the same key.
    pub fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let _guard = self.mutation_guard()?;
        let path = self.session_path(session_key);
        // Refuse to act through a non-regular session path (symlink, directory,
        // wrong extension). An absent file is not a refusal: attribution may
        // still be on disk and has to be cleaned up below.
        let metadata_path = self.metadata_path(session_key);
        let crumb_path = self.trim_breadcrumb_path(session_key);
        if validate_jsonl_session_file_path(&path).is_err() {
            // Not a regular transcript: still clear any stale sidecars so
            // provenance and breadcrumb state cannot outlive the key.
            let existed = metadata_path.exists() || crumb_path.exists();
            let _ = std::fs::remove_file(&metadata_path);
            let _ = std::fs::remove_file(&crumb_path);
            return Ok(existed);
        }
        let existed = path.exists() || metadata_path.exists() || crumb_path.exists();
        if metadata_path.exists() {
            // Remove attribution first. If deleting the message file then
            // fails, the surviving conversation is unattributed and scoped
            // access fails closed rather than inheriting stale ownership.
            std::fs::remove_file(&metadata_path)?;
        }
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        let _ = std::fs::remove_file(&crumb_path);
        Ok(existed)
    }

    /// Return the modification time of a regular session JSONL file.
    pub fn session_mtime(&self, session_key: &str) -> Option<std::time::SystemTime> {
        let path = self.session_path(session_key);
        if !is_regular_jsonl_session_file(&path) {
            return None;
        }
        std::fs::symlink_metadata(path)
            .and_then(|m| m.modified())
            .ok()
    }

    /// List all session keys that have regular JSONL files on disk.
    pub fn list_sessions(&self) -> Vec<String> {
        let entries = match std::fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if !is_regular_jsonl_session_file(&entry.path()) {
                    return None;
                }
                let name = entry.file_name().into_string().ok()?;
                name.strip_suffix(SESSION_FILE_SUFFIX)
                    .or_else(|| name.strip_suffix(METADATA_FILE_SUFFIX))
                    .map(String::from)
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

fn is_regular_jsonl_session_file(path: &Path) -> bool {
    matches!(validate_jsonl_session_file_path(path), Ok(true))
}

/// Validate that a JSONL session path is absent or an existing regular file.
/// Returns whether the regular file already exists.
fn validate_jsonl_session_file_path(path: &Path) -> std::io::Result<bool> {
    if path
        .extension()
        .is_none_or(|extension| extension != "jsonl")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session path must have a .jsonl extension",
        ));
    }

    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session path must be a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(crate) fn mutation_lock_for(sessions_dir: &Path) -> std::io::Result<Arc<MutationLock>> {
    let key = sessions_dir.canonicalize()?;
    let registry = MUTATION_LOCKS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));
    let mut locks = registry.lock();
    locks.retain(|_, record| record.migrated || record.lock.strong_count() > 0);

    if let Some(lock) = locks.get(&key).and_then(|record| record.lock.upgrade()) {
        return Ok(lock);
    }

    let migrated = locks.get(&key).is_some_and(|record| record.migrated);
    let lock = Arc::new(MutationLock::new(MutationState {
        migrated,
        receipt_state_uncertain: false,
    }));
    locks.insert(
        key,
        MutationLockRecord {
            lock: Arc::downgrade(&lock),
            migrated,
        },
    );
    Ok(lock)
}

pub(crate) fn mark_session_directory_migrated(
    sessions_dir: &Path,
    state: &mut MutationState,
) -> std::io::Result<()> {
    state.migrated = true;
    state.receipt_state_uncertain = false;
    let key = sessions_dir.canonicalize()?;
    let registry = MUTATION_LOCKS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));
    if let Some(record) = registry.lock().get_mut(&key) {
        record.migrated = true;
    }
    Ok(())
}

pub(crate) fn mark_session_directory_receipt_state_uncertain(state: &mut MutationState) {
    state.receipt_state_uncertain = true;
}

pub(crate) fn clear_session_directory_receipt_state_uncertain(state: &mut MutationState) {
    state.receipt_state_uncertain = false;
}

#[cfg(test)]
pub(crate) fn forget_session_directory_migration_state_for_test(
    sessions_dir: &Path,
) -> std::io::Result<()> {
    let key = sessions_dir.canonicalize()?;
    if let Some(registry) = MUTATION_LOCKS.get() {
        registry.lock().remove(&key);
    }
    Ok(())
}

impl SessionBackend for SessionStore {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        self.load(session_key)
    }

    fn try_load(&self, session_key: &str) -> std::io::Result<Vec<ChatMessage>> {
        self.try_load(session_key)
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        self.append(session_key, message)
    }

    fn load_if_owned(
        &self,
        session_key: &str,
        agent_alias: &str,
        channel_ids: &BTreeSet<String>,
    ) -> std::io::Result<ScopedSessionAccess<Vec<ChatMessage>>> {
        let channel_ids = channel_ids.clone();
        self.load_with_authority(session_key, agent_alias, &move |effect| {
            effect(&channel_ids)
        })
    }

    fn load_with_authority(
        &self,
        session_key: &str,
        agent_alias: &str,
        authority: &crate::session_backend::ChannelAuthority,
    ) -> std::io::Result<ScopedSessionAccess<Vec<ChatMessage>>> {
        let _guard = self.mutation_lock.lock();
        let mut result = Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "channel authority unavailable",
        ));
        authority(&mut |channel_ids| {
            result = (|| {
                let Some(metadata) = self.metadata_for_session(session_key) else {
                    return Ok(ScopedSessionAccess::Missing);
                };
                if let Err(denial) = check_session_ownership(
                    metadata.agent_alias.as_deref(),
                    metadata.channel_id.as_deref(),
                    agent_alias,
                    channel_ids,
                ) {
                    return Ok(ScopedSessionAccess::Denied(denial));
                }
                Ok(ScopedSessionAccess::Granted(self.load(session_key)))
            })();
        });
        result
    }

    fn append_if_owned(
        &self,
        session_key: &str,
        message: &ChatMessage,
        agent_alias: &str,
        channel_ids: &BTreeSet<String>,
    ) -> std::io::Result<ScopedSessionAccess<()>> {
        let channel_ids = channel_ids.clone();
        self.append_with_authority(session_key, message, agent_alias, &move |effect| {
            effect(&channel_ids)
        })
    }

    fn append_with_authority(
        &self,
        session_key: &str,
        message: &ChatMessage,
        agent_alias: &str,
        authority: &crate::session_backend::ChannelAuthority,
    ) -> std::io::Result<ScopedSessionAccess<()>> {
        let _guard = self.mutation_guard()?;
        let mut result = Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "channel authority unavailable",
        ));
        authority(&mut |channel_ids| {
            result = (|| {
                let Some(metadata) = self.metadata_for_session(session_key) else {
                    return Ok(ScopedSessionAccess::Missing);
                };
                if let Err(denial) = check_session_ownership(
                    metadata.agent_alias.as_deref(),
                    metadata.channel_id.as_deref(),
                    agent_alias,
                    channel_ids,
                ) {
                    return Ok(ScopedSessionAccess::Denied(denial));
                }
                self.append_unlocked(session_key, message)?;
                Ok(ScopedSessionAccess::Granted(()))
            })();
        });
        result
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        self.remove_last(session_key)
    }

    fn rewrite_messages(&self, session_key: &str, messages: &[ChatMessage]) -> std::io::Result<()> {
        let _guard = self.mutation_guard()?;
        self.rewrite(session_key, messages)
    }

    fn set_session_trim_breadcrumb(&self, session_key: &str, present: bool) -> std::io::Result<()> {
        self.set_trim_breadcrumb(session_key, present)
    }

    fn replace_conversation_state(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
        breadcrumb_present: bool,
    ) -> std::io::Result<()> {
        // Hold the migration fence across both writes so a concurrent
        // migration cannot interleave, and the transcript+flag pair is not
        // observed partially. Two separate files still can't be made
        // crash-atomic, so on a breadcrumb-write failure this rolls the
        // transcript back to its pre-replace content instead of leaving a
        // new transcript paired with a stale flag — a process that dies
        // between the writes can still leave the pair split, but an
        // in-process failure converges back to the last known-good state.
        let guard = self.mutation_guard()?;
        self.replace_locked(&guard, session_key, messages, breadcrumb_present)
    }

    fn replace_conversation_state_if_exists(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
        breadcrumb_present: bool,
    ) -> std::io::Result<bool> {
        // Hold the mutation guard across the existence probe and both file
        // writes. `delete_session` holds the same guard, so a deleter cannot
        // commit between the probe and the writes: once deletion wins, the
        // post-turn write is a no-op instead of recreating the transcript
        // and sidecar files the delete just removed.
        let guard = self.mutation_guard()?;
        if !is_regular_jsonl_session_file(&self.session_path(session_key)) {
            return Ok(false);
        }
        self.replace_locked(&guard, session_key, messages, breadcrumb_present)?;
        Ok(true)
    }

    fn get_session_trim_breadcrumb(&self, session_key: &str) -> std::io::Result<Option<bool>> {
        self.get_trim_breadcrumb(session_key)
    }

    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        self.update_last(session_key, message)
    }

    fn list_sessions(&self) -> Vec<String> {
        self.list_sessions()
    }

    fn list_with_authority(
        &self,
        agent_alias: &str,
        authority: &crate::session_backend::ChannelAuthority,
    ) -> std::io::Result<Vec<SessionMetadata>> {
        let _guard = self.mutation_lock.lock();
        let mut result = Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "channel authority unavailable",
        ));
        authority(&mut |channels| {
            result = Ok(self
                .list_sessions_with_metadata()
                .into_iter()
                .filter(|meta| {
                    check_session_ownership(
                        meta.agent_alias.as_deref(),
                        meta.channel_id.as_deref(),
                        agent_alias,
                        channels,
                    )
                    .is_ok()
                })
                .collect());
        });
        result
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        self.list_sessions()
            .into_iter()
            .filter_map(|key| self.metadata_for_session(&key))
            .collect()
    }

    fn compact(&self, session_key: &str) -> std::io::Result<()> {
        self.compact(session_key)
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        self.clear_messages(session_key)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        self.delete_session(session_key)
    }

    fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let _guard = self.mutation_guard()?;
        let mut updated = 0;
        for key in self.list_sessions() {
            let Some(metadata) = self.read_metadata(&key)? else {
                continue;
            };
            if metadata.agent_alias.as_deref() != Some(agent_alias) {
                continue;
            }
            self.update_metadata_unlocked(&key, |metadata| metadata.agent_alias = None)?;
            updated += 1;
        }
        Ok(updated)
    }

    fn rename_agent_attribution(&self, from: &str, to: &str) -> std::io::Result<usize> {
        let _guard = self.mutation_guard()?;
        let mut updated = 0;
        for key in self.list_sessions() {
            let Some(metadata) = self.read_metadata(&key)? else {
                continue;
            };
            if metadata.agent_alias.as_deref() != Some(from) {
                continue;
            }
            self.update_metadata_unlocked(&key, |metadata| {
                metadata.agent_alias = Some(to.to_string());
            })?;
            updated += 1;
        }
        Ok(updated)
    }

    fn count_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        self.list_sessions().into_iter().try_fold(0, |count, key| {
            let owned = self
                .read_metadata(&key)?
                .is_some_and(|metadata| metadata.agent_alias.as_deref() == Some(agent_alias));
            Ok(count + usize::from(owned))
        })
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        self.update_metadata(session_key, |metadata| {
            metadata.name = (!name.is_empty()).then(|| name.to_string());
        })
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        Ok(self
            .read_metadata(session_key)?
            .and_then(|metadata| metadata.name))
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        self.update_metadata(session_key, |metadata| {
            metadata.agent_alias = (!agent_alias.is_empty()).then(|| agent_alias.to_string());
        })
    }

    fn claim_session_with_authority(
        &self,
        session_key: &str,
        agent_alias: &str,
        authority: &crate::session_backend::ChannelAuthority,
    ) -> std::io::Result<crate::session_backend::SessionOwnerClaim> {
        use crate::session_backend::SessionOwnerClaim;
        let _guard = self.mutation_guard()?;
        let ownership = self
            .metadata_for_session(session_key)
            .map(|metadata| (metadata.agent_alias, metadata.channel_id));
        let mut claim = SessionOwnerClaim::Foreign("unavailable authority".into());
        let mut result = Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "channel authority unavailable",
        ));
        authority(&mut |channels| {
            result = (|| {
                if agent_alias.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "empty session owner",
                    ));
                }
                if let Some((owner, channel)) = &ownership
                    && let Err(denial) = check_session_ownership(
                        owner.as_deref().filter(|value| !value.is_empty()),
                        channel.as_deref().filter(|value| !value.is_empty()),
                        agent_alias,
                        channels,
                    )
                {
                    claim = SessionOwnerClaim::Foreign(format!("{denial:?}"));
                    return Ok(());
                }
                self.update_metadata_unlocked(session_key, |metadata| {
                    metadata.agent_alias = Some(agent_alias.to_string());
                })?;
                claim = SessionOwnerClaim::Claimed;
                Ok(())
            })();
        });
        result?;
        Ok(claim)
    }

    fn claim_session_agent_alias(
        &self,
        session_key: &str,
        agent_alias: &str,
    ) -> std::io::Result<crate::session_backend::SessionOwnerClaim> {
        self.claim_session_with_authority(session_key, agent_alias, &|effect| {
            effect(&BTreeSet::new())
        })
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        Ok(self
            .read_metadata(session_key)?
            .and_then(|metadata| metadata.agent_alias))
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        fn normalize(value: Option<&str>) -> Option<String> {
            value
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        }

        let channel_id = normalize(context.channel_id);
        let room_id = normalize(context.room_id);
        let sender_id = normalize(context.sender_id);
        self.update_metadata(session_key, |metadata| {
            if channel_id.is_some() {
                metadata.channel_id = channel_id;
            }
            if room_id.is_some() {
                metadata.room_id = room_id;
            }
            if sender_id.is_some() {
                metadata.sender_id = sender_id;
            }
        })
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        self.metadata_for_session(session_key)
    }

    /// Quick existence probe mirroring how `delete_session` decides whether the
    /// session is on disk: the same regular-file policy as the other JSONL
    /// session operations, plus the attribution sidecar.
    fn session_exists(&self, session_key: &str) -> bool {
        is_regular_jsonl_session_file(&self.session_path(session_key))
            || self.metadata_path(session_key).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use tempfile::TempDir;

    #[cfg(unix)]
    fn symlink_file(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(original, link)
    }

    #[cfg(windows)]
    fn symlink_file(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(original, link)
    }

    #[derive(Debug, PartialEq)]
    struct SessionEntrySnapshot {
        entry_kind: &'static str,
        link_target: Option<PathBuf>,
        entry_contents: Option<Vec<u8>>,
        tracked_target_contents: Option<Option<Vec<u8>>>,
    }

    fn snapshot_session_entry(path: &Path, tracked_target: Option<&Path>) -> SessionEntrySnapshot {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        let file_type = metadata.file_type();
        SessionEntrySnapshot {
            entry_kind: if file_type.is_symlink() {
                "symlink"
            } else if file_type.is_dir() {
                "directory"
            } else if file_type.is_file() {
                "file"
            } else {
                "other"
            },
            link_target: file_type
                .is_symlink()
                .then(|| std::fs::read_link(path).unwrap()),
            entry_contents: file_type.is_file().then(|| std::fs::read(path).unwrap()),
            tracked_target_contents: tracked_target.map(|target| std::fs::read(target).ok()),
        }
    }

    #[test]
    fn round_trip_append_and_load() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("telegram_user123", &ChatMessage::user("hello"))
            .unwrap();
        store
            .append("telegram_user123", &ChatMessage::assistant("hi there"))
            .unwrap();

        let messages = store.load("telegram_user123");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "hi there");
    }

    #[test]
    fn trim_breadcrumb_round_trips_and_survives_reopen() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        assert_eq!(store.get_trim_breadcrumb("chan_user").unwrap(), None);

        store.set_trim_breadcrumb("chan_user", true).unwrap();
        assert_eq!(store.get_trim_breadcrumb("chan_user").unwrap(), Some(true));

        // A fresh store instance simulates a process restart: the flag must
        // be a durable fact, not held only in an in-process cache.
        let reopened = SessionStore::new(tmp.path()).unwrap();
        assert_eq!(
            reopened.get_trim_breadcrumb("chan_user").unwrap(),
            Some(true)
        );

        store.set_trim_breadcrumb("chan_user", false).unwrap();
        assert_eq!(store.get_trim_breadcrumb("chan_user").unwrap(), Some(false));
    }

    #[test]
    fn get_trim_breadcrumb_rejects_an_empty_sidecar() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        std::fs::write(store.trim_breadcrumb_path("chan_user"), b"").unwrap();

        let err = store.get_trim_breadcrumb("chan_user").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn get_trim_breadcrumb_rejects_an_unrecognized_byte() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        std::fs::write(store.trim_breadcrumb_path("chan_user"), b"2").unwrap();

        let err = store.get_trim_breadcrumb("chan_user").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn get_trim_breadcrumb_rejects_trailing_garbage() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        std::fs::write(store.trim_breadcrumb_path("chan_user"), b"10").unwrap();

        let err = store.get_trim_breadcrumb("chan_user").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn deleting_a_session_removes_its_breadcrumb_flag() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store.append("chan_user", &ChatMessage::user("hi")).unwrap();
        store.set_trim_breadcrumb("chan_user", true).unwrap();

        store.delete_session("chan_user").unwrap();

        assert_eq!(store.get_trim_breadcrumb("chan_user").unwrap(), None);
    }

    #[test]
    fn load_nonexistent_session_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        let messages = store.load("nonexistent");
        assert!(messages.is_empty());
    }

    #[test]
    fn key_sanitization() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("slack/thread:123/user", &ChatMessage::user("test"))
            .unwrap();

        let messages = store.load("slack/thread:123/user");
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn sanitize_session_key_is_idempotent() {
        let raw = "slack_C123_1.2_user one";
        let once = sanitize_session_key(raw);
        let twice = sanitize_session_key(&once);
        assert_eq!(once, "slack_C123_1_2_user_one");
        assert_eq!(once, twice);
    }

    #[test]
    fn restart_simulation_matches_when_caller_pre_sanitizes() {
        let tmp = TempDir::new().unwrap();
        let runtime_key = sanitize_session_key("slack_C123_1.2_user one");

        {
            let store = SessionStore::new(tmp.path()).unwrap();
            store
                .append(&runtime_key, &ChatMessage::user("first"))
                .unwrap();
            store
                .append(&runtime_key, &ChatMessage::assistant("ack"))
                .unwrap();
        }

        let store = SessionStore::new(tmp.path()).unwrap();
        let listed = store.list_sessions();
        assert_eq!(listed, vec![runtime_key.clone()]);

        let msgs = store.load(&listed[0]);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[1].content, "ack");
    }

    #[test]
    fn list_sessions_returns_keys() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("telegram_alice", &ChatMessage::user("hi"))
            .unwrap();
        store
            .append("discord_bob", &ChatMessage::user("hey"))
            .unwrap();

        let mut sessions = store.list_sessions();
        sessions.sort();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"discord_bob".to_string()));
        assert!(sessions.contains(&"telegram_alice".to_string()));
    }

    #[test]
    fn session_operations_accept_only_regular_jsonl_files() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        let sessions_dir = tmp.path().join("sessions");

        backend
            .append("valid", &ChatMessage::user("persisted message"))
            .unwrap();
        std::fs::create_dir(sessions_dir.join("directory.jsonl")).unwrap();
        std::fs::write(sessions_dir.join("notes.txt"), "not a session").unwrap();

        let mut invalid_entries = vec![("directory", None)];

        #[cfg(any(unix, windows))]
        {
            let linked = sessions_dir.join("linked.jsonl");
            match symlink_file(&store.session_path("valid"), &linked) {
                Ok(()) => {
                    symlink_file(
                        &sessions_dir.join("missing.jsonl"),
                        &sessions_dir.join("dangling.jsonl"),
                    )
                    .unwrap();
                    invalid_entries.extend([
                        ("linked", Some(store.session_path("valid"))),
                        ("dangling", Some(sessions_dir.join("missing.jsonl"))),
                    ]);
                }
                #[cfg(windows)]
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
                Err(error) => panic!("failed to create session symlink fixture: {error}"),
            }
        }

        assert_eq!(store.list_sessions(), vec!["valid".to_string()]);
        assert!(backend.session_exists("valid"));
        assert_eq!(backend.load("valid").len(), 1);
        assert!(store.session_mtime("valid").is_some());

        for (key, _) in &invalid_entries {
            assert!(!backend.session_exists(key), "{key} must not exist");
            assert!(backend.load(key).is_empty(), "{key} must not load");
            assert!(
                store.session_mtime(key).is_none(),
                "{key} must not expose an mtime"
            );
            assert_eq!(
                backend.clear_messages(key).unwrap(),
                0,
                "{key} must not be cleared"
            );
            assert!(
                !backend.delete_session(key).unwrap(),
                "{key} must not be deleted"
            );
            assert!(
                std::fs::symlink_metadata(store.session_path(key)).is_ok(),
                "{key} filesystem entry must remain untouched"
            );
        }

        let rejected_message = ChatMessage::user("must not be persisted");
        let mut mutation_failures = Vec::new();
        for (key, tracked_target) in &invalid_entries {
            let path = store.session_path(key);
            let before = snapshot_session_entry(&path, tracked_target.as_deref());
            let append_result = backend.append(key, &rejected_message);
            let compact_result = backend.compact(key);
            let after = snapshot_session_entry(&path, tracked_target.as_deref());

            if append_result.is_ok() || compact_result.is_ok() || after != before {
                mutation_failures.push(format!(
                    "{key}: append={append_result:?}, compact={compact_result:?}, before={before:?}, after={after:?}"
                ));
            }
        }

        backend
            .append("new-session", &ChatMessage::user("new session works"))
            .unwrap();
        assert_eq!(backend.load("new-session").len(), 1);

        assert!(
            mutation_failures.is_empty(),
            "append/compact must reject invalid entries without modifying entries or targets:\n{}",
            mutation_failures.join("\n")
        );

        assert_eq!(backend.clear_messages("valid").unwrap(), 1);
        assert!(backend.session_exists("valid"));
        assert!(backend.delete_session("valid").unwrap());
        assert!(!backend.session_exists("valid"));
        assert!(sessions_dir.join("notes.txt").is_file());
    }

    #[test]
    fn append_is_truly_append_only() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "test_session";

        store.append(key, &ChatMessage::user("msg1")).unwrap();
        store.append(key, &ChatMessage::user("msg2")).unwrap();

        // Read raw file to verify append-only format
        let path = store.session_path(key);
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn remove_last_drops_final_message() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("rm_test", &ChatMessage::user("first"))
            .unwrap();
        store
            .append("rm_test", &ChatMessage::user("second"))
            .unwrap();

        assert!(store.remove_last("rm_test").unwrap());
        let messages = store.load("rm_test");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "first");
    }

    #[test]
    fn remove_last_empty_returns_false() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        assert!(!store.remove_last("nonexistent").unwrap());
    }

    #[test]
    fn update_last_via_trait_replaces_final_message() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        let key = "update_test";

        backend.append(key, &ChatMessage::user("first")).unwrap();
        backend.append(key, &ChatMessage::assistant("old")).unwrap();

        assert!(
            backend
                .update_last(key, &ChatMessage::assistant("new"))
                .unwrap()
        );

        let messages = backend.load(key);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "first");
        assert_eq!(messages[1].content, "new");
    }

    #[test]
    fn failed_rewrite_preserves_original_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "rewrite_failure";

        store.append(key, &ChatMessage::user("first")).unwrap();
        store
            .append(key, &ChatMessage::assistant("second"))
            .unwrap();
        let path = store.session_path(key);
        let original = std::fs::read(&path).unwrap();

        let mut temp_path = None;
        let result = store.rewrite_with(key, &[ChatMessage::user("replacement")], |temp, _path| {
            temp_path = Some(temp.path().to_path_buf());
            Err(std::io::Error::other("injected persist failure"))
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!temp_path.unwrap().exists());
    }

    #[test]
    fn concurrent_append_waits_for_update_last_commit() {
        let tmp = TempDir::new().unwrap();
        let update_store = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let append_store = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let key = "concurrent_update";
        update_store
            .append(key, &ChatMessage::user("first"))
            .unwrap();
        update_store
            .append(key, &ChatMessage::assistant("old"))
            .unwrap();

        let (staged_tx, staged_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let update_worker = Arc::clone(&update_store);
        let updater = std::thread::spawn(move || {
            update_worker.update_last_with(key, &ChatMessage::assistant("new"), |temp, path| {
                staged_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                temp.persist(path).map(|_| ()).map_err(|error| error.error)
            })
        });

        staged_rx.recv().unwrap();
        let (append_started_tx, append_started_rx) = mpsc::channel();
        let (append_done_tx, append_done_rx) = mpsc::channel();
        let append_store = Arc::clone(&append_store);
        let appender = std::thread::spawn(move || {
            append_started_tx.send(()).unwrap();
            let result = append_store.append(key, &ChatMessage::user("concurrent"));
            append_done_tx.send(()).unwrap();
            result
        });

        append_started_rx.recv().unwrap();
        assert!(
            append_done_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );

        release_tx.send(()).unwrap();
        assert!(updater.join().unwrap().unwrap());
        appender.join().unwrap().unwrap();

        let messages = update_store.load(key);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].content, "first");
        assert_eq!(messages[1].content, "new");
        assert_eq!(messages[2].content, "concurrent");
    }

    #[test]
    fn compact_removes_corrupt_lines() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "compact_test";

        let path = store.session_path(key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"role":"user","content":"ok"}}"#).unwrap();
        writeln!(file, "corrupt line").unwrap();
        writeln!(file, r#"{{"role":"assistant","content":"hi"}}"#).unwrap();
        drop(file);

        store.compact(key).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.trim().lines().count(), 2);
    }

    #[test]
    fn session_backend_trait_works_via_dyn() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("trait_test", &ChatMessage::user("hello"))
            .unwrap();
        let msgs = backend.load("trait_test");
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn handles_corrupt_lines_gracefully() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "corrupt_test";

        // Write valid message + corrupt line + valid message
        let path = store.session_path(key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"role":"user","content":"hello"}}"#).unwrap();
        writeln!(file, "this is not valid json").unwrap();
        writeln!(file, r#"{{"role":"assistant","content":"world"}}"#).unwrap();

        let messages = store.load(key);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].content, "world");
    }

    #[test]
    fn clear_messages_truncates_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "clear_test";

        store.append(key, &ChatMessage::user("hello")).unwrap();
        store.append(key, &ChatMessage::assistant("world")).unwrap();

        let cleared = store.clear_messages(key).unwrap();
        assert_eq!(cleared, 2);
        assert!(store.load(key).is_empty());
        // File still exists — session key remains in list_sessions
        assert!(store.session_path(key).exists());
    }

    #[test]
    fn clear_messages_empty_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        assert_eq!(store.clear_messages("nonexistent").unwrap(), 0);
    }

    #[test]
    fn clear_messages_does_not_affect_other_sessions() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        store
            .append("alice", &ChatMessage::user("alice msg"))
            .unwrap();
        store.append("bob", &ChatMessage::user("bob msg")).unwrap();

        store.clear_messages("alice").unwrap();
        assert!(store.load("alice").is_empty());
        assert_eq!(store.load("bob").len(), 1);
    }

    #[test]
    fn clear_messages_then_append_works() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "reuse_test";

        store.append(key, &ChatMessage::user("old")).unwrap();
        store.clear_messages(key).unwrap();
        store.append(key, &ChatMessage::user("new")).unwrap();

        let messages = store.load(key);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "new");
    }

    #[test]
    fn clear_messages_removes_trim_breadcrumb_sidecar() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "crumb_reset_test";

        store.append(key, &ChatMessage::user("hello")).unwrap();
        store.set_trim_breadcrumb(key, true).unwrap();
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), Some(true));

        store.clear_messages(key).unwrap();

        // A reset session has no synthetic marker; the recorded flag must
        // not survive as a stale `true` for the next first message.
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), None);
    }

    #[test]
    fn clear_messages_removes_stale_sidecar_even_when_already_empty() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "already_empty_crumb_test";

        store.append(key, &ChatMessage::user("hello")).unwrap();
        store.set_trim_breadcrumb(key, true).unwrap();
        store.clear_messages(key).unwrap();
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), None);

        // Clearing an already-empty session must not leave a stale flag
        // behind either.
        store.set_trim_breadcrumb(key, true).unwrap();
        assert_eq!(store.clear_messages(key).unwrap(), 0);
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), None);
    }

    #[test]
    fn clear_messages_removes_sidecar_only_breadcrumb_with_no_transcript_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "sidecar_only_crumb_test";

        // No transcript file exists at all for this key (e.g. after a prior
        // cleanup removed the transcript but not its sidecar), yet the
        // sidecar is independently creatable.
        store.set_trim_breadcrumb(key, true).unwrap();
        assert!(!is_regular_jsonl_session_file(&store.session_path(key)));
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), Some(true));

        assert_eq!(store.clear_messages(key).unwrap(), 0);
        assert_eq!(
            store.get_trim_breadcrumb(key).unwrap(),
            None,
            "a sidecar-only breadcrumb must not survive clear_messages just because \
             there was no transcript file to early-return past"
        );

        // A session that later receives its first message must not inherit
        // the stale flag and misclassify that message as post-trim.
        store
            .append(key, &ChatMessage::user("first message"))
            .unwrap();
        assert_eq!(store.get_trim_breadcrumb(key).unwrap(), None);
    }

    #[test]
    fn delete_session_removes_jsonl_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "delete_test";

        store.append(key, &ChatMessage::user("hello")).unwrap();
        assert_eq!(store.load(key).len(), 1);

        let deleted = store.delete_session(key).unwrap();
        assert!(deleted);
        assert!(store.load(key).is_empty());
        assert!(!store.session_path(key).exists());
    }

    #[test]
    fn delete_session_nonexistent_returns_false() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        let deleted = store.delete_session("nonexistent").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn delete_session_via_trait() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("trait_delete", &ChatMessage::user("hello"))
            .unwrap();
        assert_eq!(backend.load("trait_delete").len(), 1);

        let deleted = backend.delete_session("trait_delete").unwrap();
        assert!(deleted);
        assert!(backend.load("trait_delete").is_empty());
    }

    // ── session_exists─────────────────────────────────────
    #[test]
    fn session_exists_tracks_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        assert!(!backend.session_exists("ghost"));

        backend
            .append("ghost", &ChatMessage::user("first"))
            .unwrap();
        assert!(backend.session_exists("ghost"));

        backend.delete_session("ghost").unwrap();
        assert!(!backend.session_exists("ghost"));
    }

    #[test]
    fn replace_conversation_state_if_exists_is_a_no_op_once_deletion_wins() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend.append("gone", &ChatMessage::user("turn")).unwrap();
        assert!(backend.delete_session("gone").unwrap());

        // Deterministic delete-versus-completion ordering: the deleter (which
        // holds the same mutation guard) commits first, then the post-turn
        // write arrives. It must become a no-op instead of recreating the
        // transcript and sidecar files the delete just removed.
        let written = backend
            .replace_conversation_state_if_exists("gone", &[ChatMessage::user("late turn")], false)
            .unwrap();
        assert!(
            !written,
            "post-turn write must be a no-op once deletion wins"
        );
        assert!(!backend.session_exists("gone"));
        assert!(backend.load("gone").is_empty());

        // The live path still writes transcript and flag together.
        backend.append("live", &ChatMessage::user("turn")).unwrap();
        let written = backend
            .replace_conversation_state_if_exists("live", &[ChatMessage::user("new")], true)
            .unwrap();
        assert!(written);
        assert_eq!(backend.load("live").len(), 1);
        assert_eq!(
            backend.get_session_trim_breadcrumb("live").unwrap(),
            Some(true)
        );
    }

    // ── get_session_metadata (trait default) tests ──────────────────

    #[test]
    fn get_session_metadata_returns_none_for_missing() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        assert!(backend.get_session_metadata("nonexistent").is_none());
    }

    #[test]
    fn get_session_metadata_returns_correct_count() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("test_session", &ChatMessage::user("hello"))
            .unwrap();
        backend
            .append("test_session", &ChatMessage::assistant("hi"))
            .unwrap();

        let meta = backend.get_session_metadata("test_session").unwrap();
        assert_eq!(meta.key, "test_session");
        assert_eq!(meta.message_count, 2);
        assert!(meta.name.is_none());
    }

    #[test]
    fn ownership_metadata_survives_restart_and_precedes_first_message() {
        let tmp = TempDir::new().unwrap();
        let key = "discord_room_42";

        {
            let store = SessionStore::new(tmp.path()).unwrap();
            let backend: &dyn SessionBackend = &store;
            backend.set_session_name(key, "Operations").unwrap();
            backend.set_session_agent_alias(key, "rowan").unwrap();
            backend
                .set_session_context(
                    key,
                    SessionContext {
                        channel_id: Some("discord.primary"),
                        room_id: Some("42"),
                        sender_id: Some("operator"),
                    },
                )
                .unwrap();

            assert!(backend.session_exists(key));
            assert_eq!(backend.list_sessions(), vec![key.to_string()]);
            backend
                .append(key, &ChatMessage::user("restart-safe"))
                .unwrap();
        }

        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        let metadata = backend.get_session_metadata(key).unwrap();
        assert_eq!(metadata.name.as_deref(), Some("Operations"));
        assert_eq!(metadata.agent_alias.as_deref(), Some("rowan"));
        assert_eq!(metadata.channel_id.as_deref(), Some("discord.primary"));
        assert_eq!(metadata.room_id.as_deref(), Some("42"));
        assert_eq!(metadata.sender_id.as_deref(), Some("operator"));
        assert_eq!(metadata.message_count, 1);
    }

    #[test]
    fn legacy_message_file_remains_unattributed() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        store
            .append("legacy", &ChatMessage::user("pre-metadata"))
            .unwrap();

        let metadata = (&store as &dyn SessionBackend)
            .get_session_metadata("legacy")
            .unwrap();
        assert!(metadata.agent_alias.is_none());
        assert!(metadata.channel_id.is_none());
    }

    #[test]
    fn agent_attribution_lifecycle_updates_sidecars() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        for key in ["one", "two"] {
            backend.append(key, &ChatMessage::user(key)).unwrap();
            backend.set_session_agent_alias(key, "rowan").unwrap();
        }

        assert_eq!(backend.count_agent_attribution("rowan").unwrap(), 2);
        assert_eq!(
            backend.rename_agent_attribution("rowan", "sable").unwrap(),
            2
        );
        assert_eq!(backend.count_agent_attribution("rowan").unwrap(), 0);
        assert_eq!(backend.count_agent_attribution("sable").unwrap(), 2);
        assert_eq!(backend.clear_agent_attribution("sable").unwrap(), 2);
        assert_eq!(backend.count_agent_attribution("sable").unwrap(), 0);
    }

    #[test]
    fn ownership_claim_materializes_an_empty_transcript() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .set_session_agent_alias("claimed-before-first-message", "rowan")
            .unwrap();

        let transcript = store.session_path("claimed-before-first-message");
        assert!(transcript.exists());
        assert_eq!(std::fs::metadata(transcript).unwrap().len(), 0);
        let metadata = backend
            .get_session_metadata("claimed-before-first-message")
            .unwrap();
        assert_eq!(metadata.agent_alias.as_deref(), Some("rowan"));
        assert_eq!(metadata.message_count, 0);
    }

    #[test]
    fn delete_session_removes_ownership_sidecar() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        backend
            .append("owned", &ChatMessage::user("private"))
            .unwrap();
        backend.set_session_agent_alias("owned", "rowan").unwrap();
        let metadata_path = store.metadata_path("owned");
        assert!(metadata_path.exists());

        assert!(backend.delete_session("owned").unwrap());
        assert!(!metadata_path.exists());
        assert!(!backend.session_exists("owned"));
    }

    #[test]
    fn blocked_sidecar_rename_leaves_no_live_metadata_behind() {
        // The migrated marker path is occupied by a directory, so the rename
        // cannot succeed. The transcript is already archived at that point,
        // so the sidecar must not survive as a metadata-only session.
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;
        backend
            .append("owned", &ChatMessage::user("private"))
            .unwrap();
        backend.set_session_agent_alias("owned", "rowan").unwrap();
        let sessions_dir = tmp.path().join("sessions");
        let sidecar = metadata_sidecar_path(&sessions_dir, "owned");
        assert!(sidecar.exists());
        std::fs::create_dir_all(sessions_dir.join(format!(
            "{}{}",
            sanitize_session_key("owned"),
            JSONL_SESSION_MIGRATED_METADATA_FILE_SUFFIX
        )))
        .unwrap();

        mark_metadata_sidecar_migrated(&sessions_dir, "owned").unwrap();

        assert!(!sidecar.exists());
        assert!(
            read_metadata_sidecar(&sessions_dir, "owned")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn replace_conversation_state_rolls_back_transcript_when_breadcrumb_write_fails() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("s1", &ChatMessage::user("pre-replace turn"))
            .unwrap();
        backend.set_session_trim_breadcrumb("s1", false).unwrap();

        // Force the breadcrumb half of the write to fail by occupying its
        // path with a directory: `fs::write` errors instead of replacing it.
        let breadcrumb_path = store.trim_breadcrumb_path("s1");
        std::fs::remove_file(&breadcrumb_path).unwrap();
        std::fs::create_dir(&breadcrumb_path).unwrap();

        let result = backend.replace_conversation_state(
            "s1",
            &[ChatMessage::user(
                "replacement turn that must not land alone",
            )],
            true,
        );
        assert!(
            result.is_err(),
            "the poisoned breadcrumb path must fail the call"
        );

        // The transcript must have rolled back to its pre-replace content,
        // not the replacement that could never be paired with a committed
        // flag.
        let messages = backend.load("s1");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "pre-replace turn");
    }
    #[test]
    fn live_authority_resolves_after_actual_storage_wait() {
        use std::sync::{Arc, mpsc};
        use std::time::Duration;
        for operation in ["load", "append", "list", "claim"] {
            let tmp = tempfile::tempdir().unwrap();
            let store = Arc::new(SessionStore::new(tmp.path()).unwrap());
            store
                .append("legacy", &ChatMessage::user("private bytes"))
                .unwrap();
            store
                .set_session_context(
                    "legacy",
                    crate::session_backend::SessionContext {
                        channel_id: Some("discord.owner"),
                        ..Default::default()
                    },
                )
                .unwrap();
            let policy = Arc::new(parking_lot::RwLock::new(BTreeSet::from([
                "discord.owner".to_string()
            ])));
            let blocked = store.mutation_lock.lock();
            let (entered_tx, entered_rx) = mpsc::channel();
            let worker_store = Arc::clone(&store);
            let worker_policy = Arc::clone(&policy);
            let worker = std::thread::spawn(move || {
                let authority = move |effect: &mut dyn FnMut(&BTreeSet<String>)| {
                    let grant = worker_policy.read();
                    entered_tx.send(()).unwrap();
                    effect(&grant);
                };
                match operation {
                    "load" => assert!(matches!(
                        worker_store
                            .load_with_authority("legacy", "owner", &authority)
                            .unwrap(),
                        ScopedSessionAccess::Denied(_)
                    )),
                    "append" => assert!(matches!(
                        worker_store
                            .append_with_authority(
                                "legacy",
                                &ChatMessage::user("forbidden"),
                                "owner",
                                &authority
                            )
                            .unwrap(),
                        ScopedSessionAccess::Denied(_)
                    )),
                    "list" => assert!(
                        worker_store
                            .list_with_authority("owner", &authority)
                            .unwrap()
                            .is_empty()
                    ),
                    "claim" => assert!(matches!(
                        worker_store
                            .claim_session_with_authority("legacy", "owner", &authority)
                            .unwrap(),
                        crate::session_backend::SessionOwnerClaim::Foreign(_)
                    )),
                    _ => unreachable!(),
                }
            });
            assert!(
                entered_rx.recv_timeout(Duration::from_millis(50)).is_err(),
                "policy must not be captured ahead of storage"
            );
            policy.write().clear();
            drop(blocked);
            worker.join().unwrap();
            assert_eq!(store.load("legacy").len(), 1);
            assert_eq!(store.get_session_agent_alias("legacy").unwrap(), None);
            let grant = BTreeSet::from(["discord.owner".to_string()]);
            assert!(matches!(
                store.load_if_owned("legacy", "owner", &grant).unwrap(),
                ScopedSessionAccess::Granted(_)
            ));
            assert!(matches!(
                store
                    .append_if_owned("legacy", &ChatMessage::user("allowed"), "owner", &grant)
                    .unwrap(),
                ScopedSessionAccess::Granted(())
            ));
        }
    }
}
