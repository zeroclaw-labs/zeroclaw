//! Legacy JSONL test fixture for master-format migration tests.

use super::session_backend::SessionBackend;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use zeroclaw_api::model_provider::ChatMessage;
pub use zeroclaw_api::session_keys::sanitize_session_key;

type MutationLock = parking_lot::Mutex<()>;
static MUTATION_LOCKS: OnceLock<parking_lot::Mutex<HashMap<PathBuf, Weak<MutationLock>>>> =
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
        Ok(Self {
            sessions_dir,
            mutation_lock,
        })
    }

    /// Compute the file path for a session key, sanitizing for filesystem safety.
    fn session_path(&self, session_key: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("{}.jsonl", sanitize_session_key(session_key)))
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
    /// trim breadcrumb. Respects the native installation migration fence.
    pub fn set_trim_breadcrumb(&self, session_key: &str, present: bool) -> std::io::Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let _guard = self.mutation_guard();
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let _guard = self.mutation_guard();
        self.append_unlocked(session_key, message)
    }

    fn mutation_guard(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.mutation_lock.lock()
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let _guard = self.mutation_guard();
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
        self.update_last_with(session_key, message, publish_staged_rewrite)
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let _guard = self.mutation_guard();
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let _guard = self.mutation_guard();
        validate_jsonl_session_file_path(&self.session_path(session_key))?;
        let messages = self.load(session_key);
        self.rewrite(session_key, &messages)
    }

    fn rewrite(&self, session_key: &str, messages: &[ChatMessage]) -> std::io::Result<()> {
        self.rewrite_with(session_key, messages, publish_staged_rewrite)
    }

    /// Transcript+sidecar replacement assuming the caller already holds the
    /// mutation guard. Split out so the existence-guarded
    /// `replace_conversation_state_if_exists` can probe-then-write under one
    /// guard acquisition (`parking_lot` mutexes are not reentrant, so the
    /// trait methods cannot call each other while holding it).
    fn replace_locked(
        &self,
        _guard: &parking_lot::MutexGuard<'_, ()>,
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
        let mut temp = tempfile::Builder::new().make_in(&self.sessions_dir, create_staging_file)?;
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let _guard = self.mutation_guard();
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let _guard = self.mutation_guard();
        let path = self.session_path(session_key);
        let crumb_path = self.trim_breadcrumb_path(session_key);
        if !is_regular_jsonl_session_file(&path) {
            let had_crumb = crumb_path.exists();
            let _ = std::fs::remove_file(&crumb_path);
            return Ok(had_crumb);
        }
        std::fs::remove_file(&path)?;
        let _ = std::fs::remove_file(crumb_path);
        Ok(true)
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
                name.strip_suffix(".jsonl").map(String::from)
            })
            .collect()
    }
}

/// Rename a staged rewrite over the live session file.
///
/// Readers take no lock, so one may hold the session file open at this
/// moment, and that must not stop the replace on any platform. On Windows
/// `persist` is a bare `MoveFileExW`, which cannot replace a file that any
/// reader holds open. `std::fs::rename` retries that refusal with POSIX
/// semantics, which swap the name while open readers keep the old contents,
/// as long as they opened it with delete sharing (std's own opens do). On Unix
/// both are rename(2).
fn publish_staged_rewrite(temp: tempfile::NamedTempFile, path: &Path) -> std::io::Result<()> {
    let mut staged = temp.into_temp_path();
    std::fs::rename(&staged, path)?;
    // The staging name no longer exists, so there is nothing left to clean up.
    staged.disable_cleanup(true);
    Ok(())
}

/// Create a staging file the way `NamedTempFile::new_in` does (exclusively,
/// owner-only on Unix) but without the Windows temporary attribute: `persist`
/// would clear that attribute, and the plain rename that publishes the file
/// would carry it onto the session file.
fn create_staging_file(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
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

fn mutation_lock_for(sessions_dir: &Path) -> std::io::Result<Arc<MutationLock>> {
    let key = sessions_dir.canonicalize()?;
    let registry = MUTATION_LOCKS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));
    let mut locks = registry.lock();
    locks.retain(|_, record| record.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(MutationLock::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

impl SessionBackend for SessionStore {
    fn require_legacy_access(&self) -> std::io::Result<()> {
        crate::session_events::installation::legacy_guard(&self.sessions_dir).map(|_| ())
    }

    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        self.load(session_key)
    }

    fn try_load(&self, session_key: &str) -> std::io::Result<Vec<ChatMessage>> {
        self.try_load(session_key)
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        self.append(session_key, message)
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        self.remove_last(session_key)
    }

    fn rewrite_messages(&self, session_key: &str, messages: &[ChatMessage]) -> std::io::Result<()> {
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let _guard = self.mutation_guard();
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let guard = self.mutation_guard();
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
        let _family_guard = crate::session_events::installation::legacy_guard(&self.sessions_dir)?;
        let guard = self.mutation_guard();
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

    fn list_sessions_with_metadata(&self) -> Vec<super::session_backend::SessionMetadata> {
        use chrono::{DateTime, Utc};
        self.list_sessions()
            .into_iter()
            .map(|key| {
                let last_activity: DateTime<Utc> = self
                    .session_mtime(&key)
                    .map(DateTime::<Utc>::from)
                    .unwrap_or_else(Utc::now);
                super::session_backend::SessionMetadata {
                    title: None,
                    created_at: last_activity,
                    last_activity,
                    message_count: 0,
                    key,
                    agent_alias: None,
                    channel_id: None,
                    room_id: None,
                    sender_id: None,
                    principal_id: None,
                }
            })
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

    /// Quick existence probe using the same regular-file policy as the other
    /// JSONL session operations.
    fn session_exists(&self, session_key: &str) -> bool {
        is_regular_jsonl_session_file(&self.session_path(session_key))
    }
}
