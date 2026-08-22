//! JSONL-based session persistence for channel conversations.

use crate::session_backend::SessionBackend;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use zeroclaw_api::model_provider::ChatMessage;
pub use zeroclaw_api::session_keys::sanitize_session_key;

#[derive(Default)]
pub(crate) struct MutationState {
    migrated: bool,
    receipt_state_uncertain: bool,
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
        self.sessions_dir
            .join(format!("{}.jsonl", sanitize_session_key(session_key)))
    }

    fn incomplete_marker_path(&self, session_key: &str) -> PathBuf {
        self.sessions_dir.join(format!(
            "{}.jsonl.incomplete",
            sanitize_session_key(session_key)
        ))
    }

    fn sync_sessions_dir(&self) -> std::io::Result<()> {
        sync_directory(&self.sessions_dir)
    }

    /// Durably record that a transcript mutation is in progress.
    pub fn mark_transcript_incomplete(&self, session_key: &str) -> std::io::Result<()> {
        let _guard = self.mutation_lock.lock();
        let marker_path = self.incomplete_marker_path(session_key);
        let mut marker = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(marker_path)?;
        marker.write_all(b"incomplete\n")?;
        marker.sync_all()?;
        self.sync_sessions_dir()
    }

    /// Clear a completed transcript mutation's durable intent marker.
    ///
    /// Removing the marker is the transcript's commit point, so the session
    /// file's own data blocks must reach stable storage *first*. Appends are
    /// buffered by the OS, and the directory fsync only covers directory
    /// entries, so clearing the marker without this step could leave a crash
    /// ordering where the durable marker removal outlives the messages it was
    /// protecting: on restart the transcript would look complete while missing
    /// the appended rows. Any sync failure keeps the marker in place.
    pub fn clear_transcript_incomplete(&self, session_key: &str) -> std::io::Result<()> {
        self.clear_transcript_incomplete_with(session_key, sync_file)
    }

    fn clear_transcript_incomplete_with<F>(&self, session_key: &str, sync: F) -> std::io::Result<()>
    where
        F: FnOnce(&Path) -> std::io::Result<()>,
    {
        let _guard = self.mutation_lock.lock();
        let marker_path = self.incomplete_marker_path(session_key);
        if !marker_path.exists() {
            return Ok(());
        }

        sync(&self.session_path(session_key))?;

        match std::fs::remove_file(&marker_path) {
            Ok(()) => self.sync_sessions_dir(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Check whether an earlier transcript mutation left a durable marker.
    pub fn transcript_incomplete(&self, session_key: &str) -> std::io::Result<bool> {
        match std::fs::metadata(self.incomplete_marker_path(session_key)) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Load all messages for a session from its JSONL file.
    /// Returns an empty vec if the file does not exist or is unreadable.
    pub fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let path = self.session_path(session_key);
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
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
        let messages = self.load(session_key);
        self.rewrite(session_key, &messages)
    }

    fn rewrite(&self, session_key: &str, messages: &[ChatMessage]) -> std::io::Result<()> {
        self.rewrite_with(session_key, messages, |temp, path| {
            temp.persist(path).map(|_| ()).map_err(|error| error.error)
        })
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
    pub fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let _guard = self.mutation_guard()?;
        let count = self.load(session_key).len();
        if count > 0 {
            self.rewrite(session_key, &[])?;
        }
        Ok(count)
    }

    /// Delete a session's JSONL file. Returns `true` if the file existed.
    pub fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let _guard = self.mutation_guard()?;
        let path = self.session_path(session_key);
        let marker_path = self.incomplete_marker_path(session_key);
        let mut deleted = false;
        match std::fs::remove_file(&path) {
            Ok(()) => deleted = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match std::fs::remove_file(&marker_path) {
            Ok(()) => deleted = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if deleted {
            self.sync_sessions_dir()?;
        }
        Ok(deleted)
    }

    /// Return the modification time of a session's JSONL file.
    pub fn session_mtime(&self, session_key: &str) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.session_path(session_key))
            .or_else(|_| std::fs::metadata(self.incomplete_marker_path(session_key)))
            .and_then(|m| m.modified())
            .ok()
    }

    /// List all session keys that have files on disk.
    pub fn list_sessions(&self) -> Vec<String> {
        let entries = match std::fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().into_string().ok()?;
                name.strip_suffix(".jsonl.incomplete")
                    .or_else(|| name.strip_suffix(".jsonl"))
                    .map(String::from)
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
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

/// Flush a session file's own data blocks so an append is durable.
///
/// Returns `Ok(())` when the file does not exist: a mutation can be marked and
/// then fail before its first append ever creates the JSONL file, and there is
/// no data to lose in that case.
fn sync_file(path: &Path) -> std::io::Result<()> {
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(file) => file.sync_all(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Flush a directory's metadata so a just-created or just-removed entry is durable.
///
/// This only makes the directory entry itself survive a crash. Callers are
/// responsible for syncing the entry's file contents first — see
/// [`SessionStore::clear_transcript_incomplete`], which syncs the session file
/// before removing its marker so the commit point cannot outrun the data.
#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// Windows variant of [`sync_directory`].
///
/// Opening a directory handle requires `FILE_FLAG_BACKUP_SEMANTICS`, and
/// `FlushFileBuffers` on a directory handle returns `ERROR_ACCESS_DENIED`
/// (OS error 5) because NTFS does not expose Unix-style directory metadata
/// flushing. The entry's file was already synced, so that expected error is
/// non-fatal; every other error still propagates.
#[cfg(windows)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    match dir.sync_all() {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(5) => {
            ::zeroclaw_log::record!(
                TRACE,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "Ignoring expected ACCESS_DENIED when fsyncing directory on Windows: {}",
                    path.display()
                )
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Fallback for targets that expose neither directory-fsync mechanism.
#[cfg(not(any(unix, windows)))]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    let _ = path;
    Ok(())
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

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        self.append(session_key, message)
    }

    fn mark_transcript_incomplete(&self, session_key: &str) -> std::io::Result<()> {
        self.mark_transcript_incomplete(session_key)
    }

    fn clear_transcript_incomplete(&self, session_key: &str) -> std::io::Result<()> {
        self.clear_transcript_incomplete(session_key)
    }

    fn transcript_incomplete(&self, session_key: &str) -> std::io::Result<bool> {
        self.transcript_incomplete(session_key)
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        self.remove_last(session_key)
    }

    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        self.update_last(session_key, message)
    }

    fn list_sessions(&self) -> Vec<String> {
        self.list_sessions()
    }

    fn list_sessions_with_metadata(&self) -> Vec<crate::session_backend::SessionMetadata> {
        use chrono::{DateTime, Utc};
        self.list_sessions()
            .into_iter()
            .map(|key| {
                let last_activity: DateTime<Utc> = self
                    .session_mtime(&key)
                    .map(DateTime::<Utc>::from)
                    .unwrap_or_else(Utc::now);
                crate::session_backend::SessionMetadata {
                    name: None,
                    created_at: last_activity,
                    last_activity,
                    message_count: 0,
                    key,
                    agent_alias: None,
                    channel_id: None,
                    room_id: None,
                    sender_id: None,
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

    /// Quick existence probe mirroring how `delete_session` decides whether
    /// the session is on disk Checking file presence is the same
    /// O(1) `stat` that `delete_session` itself performs.
    fn session_exists(&self, session_key: &str) -> bool {
        self.session_path(session_key).exists() || self.incomplete_marker_path(session_key).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use tempfile::TempDir;

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
    fn transcript_intent_marker_survives_restart_until_cleared() {
        let tmp = TempDir::new().unwrap();
        let key = "restart_incomplete";

        {
            let store = SessionStore::new(tmp.path()).unwrap();
            store.mark_transcript_incomplete(key).unwrap();
            store.append(key, &ChatMessage::user("partial")).unwrap();
            assert!(store.transcript_incomplete(key).unwrap());
        }

        let store = SessionStore::new(tmp.path()).unwrap();
        assert!(store.transcript_incomplete(key).unwrap());
        store.clear_transcript_incomplete(key).unwrap();
        assert!(!store.transcript_incomplete(key).unwrap());
    }

    #[test]
    fn clear_transcript_incomplete_syncs_the_transcript_before_the_marker() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "durable_commit";

        store.mark_transcript_incomplete(key).unwrap();
        store.append(key, &ChatMessage::user("committed")).unwrap();

        let synced = std::cell::Cell::new(None);
        store
            .clear_transcript_incomplete_with(key, |path| {
                // The marker must still be present at the moment the
                // transcript is synced — that ordering is the commit point.
                synced.set(Some(path.to_path_buf()));
                assert!(
                    path.with_extension("jsonl.incomplete").exists()
                        || store.incomplete_marker_path(key).exists(),
                    "marker cleared before the transcript was synced"
                );
                Ok(())
            })
            .unwrap();

        assert_eq!(
            synced.into_inner().as_deref(),
            Some(store.session_path(key).as_path())
        );
        assert!(!store.transcript_incomplete(key).unwrap());
    }

    #[test]
    fn a_failed_transcript_sync_keeps_the_poison_marker_set() {
        let tmp = TempDir::new().unwrap();
        let key = "unsynced_commit";

        {
            let store = SessionStore::new(tmp.path()).unwrap();
            store.mark_transcript_incomplete(key).unwrap();
            store.append(key, &ChatMessage::user("at risk")).unwrap();

            let error = store
                .clear_transcript_incomplete_with(key, |_| {
                    Err(std::io::Error::other("simulated fsync failure"))
                })
                .expect_err("an unsynced transcript must not be committed");
            assert_eq!(error.kind(), std::io::ErrorKind::Other);
            assert!(store.transcript_incomplete(key).unwrap());
        }

        // Restart: the marker outlived the process, so the transcript is
        // still correctly treated as poisoned rather than complete.
        let store = SessionStore::new(tmp.path()).unwrap();
        assert!(store.transcript_incomplete(key).unwrap());
    }

    #[test]
    fn clear_transcript_incomplete_tolerates_a_marked_session_with_no_file_yet() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "marked_but_empty";

        store.mark_transcript_incomplete(key).unwrap();
        assert!(!store.session_path(key).exists());

        store.clear_transcript_incomplete(key).unwrap();
        assert!(!store.transcript_incomplete(key).unwrap());
    }

    #[test]
    fn delete_session_removes_marker_even_without_messages() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "marker_only";

        store.mark_transcript_incomplete(key).unwrap();
        assert!(store.session_exists(key));
        assert_eq!(store.list_sessions(), vec![key.to_string()]);
        assert!(store.delete_session(key).unwrap());
        assert!(!store.transcript_incomplete(key).unwrap());
        assert!(!store.delete_session(key).unwrap());
    }

    #[test]
    fn transcript_marker_lifecycle_syncs_sessions_dir_on_every_platform() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "marker_lifecycle";

        // Directory sync runs on the first-turn persistence path; on Windows the
        // expected directory-flush refusal must not fail the mutation.
        store.mark_transcript_incomplete(key).unwrap();
        assert!(store.transcript_incomplete(key).unwrap());

        store.append(key, &ChatMessage::user("first turn")).unwrap();
        assert_eq!(store.load(key).len(), 1);

        store.clear_transcript_incomplete(key).unwrap();
        assert!(!store.transcript_incomplete(key).unwrap());

        assert!(store.delete_session(key).unwrap());
        assert!(!store.session_path(key).exists());

        // The helper itself must succeed against a real directory handle.
        sync_directory(tmp.path().join("sessions").as_path()).unwrap();
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
}
