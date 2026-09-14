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

    /// Load all messages for a session from its JSONL file.
    /// Returns an empty vec if the path is not a regular JSONL session file or
    /// the file is unreadable.
    pub fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let path = self.session_path(session_key);
        if !is_regular_jsonl_session_file(&path) {
            return Vec::new();
        }
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
        if !is_regular_jsonl_session_file(&self.session_path(session_key)) {
            return Ok(0);
        }
        let count = self.load(session_key).len();
        if count > 0 {
            self.rewrite(session_key, &[])?;
        }
        Ok(count)
    }

    /// Delete a regular session JSONL file. Returns `true` if the file existed.
    pub fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let _guard = self.mutation_guard()?;
        let path = self.session_path(session_key);
        if !is_regular_jsonl_session_file(&path) {
            return Ok(false);
        }
        std::fs::remove_file(&path)?;
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

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        self.append(session_key, message)
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
