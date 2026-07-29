//! JSONL-based session persistence for channel conversations.

use crate::session_backend::SessionBackend;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use zeroclaw_api::model_provider::ChatMessage;
pub use zeroclaw_api::session_keys::sanitize_session_key;

/// Append-only JSONL session store for channel conversations.
pub struct SessionStore {
    sessions_dir: PathBuf,
    /// Serializes ownership compare-and-set so two concurrent
    /// `claim_session_agent_alias` calls cannot both observe "no owner".
    claim_lock: std::sync::Mutex<()>,
}

/// Sidecar metadata stored alongside a session's JSONL file.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SessionMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl SessionStore {
    /// Create a new session store, ensuring the sessions directory exists.
    pub fn new(workspace_dir: &Path) -> std::io::Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        Ok(Self {
            sessions_dir,
            claim_lock: std::sync::Mutex::new(()),
        })
    }

    /// Compute the file path for a session key, sanitizing for filesystem safety.
    fn session_path(&self, session_key: &str) -> PathBuf {
        self.sessions_dir
            .join(format!("{}.jsonl", sanitize_session_key(session_key)))
    }

    /// Compute the sidecar metadata path for a session key.
    fn meta_path(&self, session_key: &str) -> std::path::PathBuf {
        let mut p = self.session_path(session_key);
        p.set_extension("meta.json");
        p
    }

    /// Read the sidecar metadata for a session, returning a default (empty)
    /// meta when the sidecar does not yet exist. Corrupted sidecars are a hard
    /// error so ownership writes fail closed rather than silently resetting.
    fn read_meta(&self, session_key: &str) -> std::io::Result<SessionMeta> {
        match std::fs::read_to_string(self.meta_path(session_key)) {
            Ok(json) => serde_json::from_str(&json).map_err(|e| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "file": self.meta_path(session_key).display().to_string(),
                            "error": format!("{e}"),
                        })),
                    "Corrupted session metadata file"
                );
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "corrupted session metadata",
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SessionMeta::default()),
            Err(e) => Err(e),
        }
    }

    /// Serialize and persist the sidecar metadata for a session.
    fn write_meta(&self, session_key: &str, meta: &SessionMeta) -> std::io::Result<()> {
        let json = serde_json::to_string(meta)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(self.meta_path(session_key), json)
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
        let mut messages = self.load(session_key);
        if messages.is_empty() {
            return Ok(false);
        }
        messages.pop();
        self.rewrite(session_key, &messages)?;
        Ok(true)
    }

    /// Compact a session file by rewriting only valid messages (removes corrupt lines).
    pub fn compact(&self, session_key: &str) -> std::io::Result<()> {
        let messages = self.load(session_key);
        self.rewrite(session_key, &messages)
    }

    fn rewrite(&self, session_key: &str, messages: &[ChatMessage]) -> std::io::Result<()> {
        let path = self.session_path(session_key);
        let mut file = std::fs::File::create(&path)?;
        for msg in messages {
            let json = serde_json::to_string(msg)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            writeln!(file, "{json}")?;
        }
        Ok(())
    }

    /// Clear all messages from a session by truncating its JSONL file.
    /// The file is preserved (empty) so the session key remains in `list_sessions`.
    pub fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let count = self.load(session_key).len();
        if count > 0 {
            self.rewrite(session_key, &[])?;
        }
        Ok(count)
    }

    /// Delete a session's JSONL file and its sidecar metadata. Returns `true`
    /// if either file existed (i.e., the session had any on-disk state).
    pub fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let jsonl_path = self.session_path(session_key);
        let meta_path = self.meta_path(session_key);

        let jsonl_existed = jsonl_path.exists();
        let meta_existed = meta_path.exists();

        if jsonl_existed {
            std::fs::remove_file(&jsonl_path)?;
        }
        // Clean up the sidecar. NotFound is treated as success (benign
        // race); all other errors (e.g. PermissionDenied) are propagated.
        if meta_existed
            && let Err(e) = std::fs::remove_file(&meta_path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(e);
        }

        Ok(jsonl_existed || meta_existed)
    }

    /// Return the modification time of a session's JSONL file.
    pub fn session_mtime(&self, session_key: &str) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.session_path(session_key))
            .and_then(|m| m.modified())
            .ok()
    }

    /// List all session keys that have files on disk.
    pub fn list_sessions(&self) -> Vec<String> {
        let entries = match std::fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut sessions: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if let Some(key) = name.strip_suffix(".jsonl") {
                sessions.insert(key.to_string());
            } else if let Some(key) = name.strip_suffix(".meta.json") {
                sessions.insert(key.to_string());
            }
        }
        let mut result: Vec<String> = sessions.into_iter().collect();
        result.sort();
        result
    }
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
                let mut meta = crate::session_backend::SessionMetadata {
                    name: None,
                    created_at: last_activity,
                    last_activity,
                    message_count: 0,
                    key,
                    agent_alias: None,
                    channel_id: None,
                    room_id: None,
                    sender_id: None,
                };
                // Hydrate agent_alias and name from the sidecar .meta.json.
                if let Ok(json) = std::fs::read_to_string(self.meta_path(&meta.key))
                    && let Ok(sidecar) = serde_json::from_str::<SessionMeta>(&json)
                {
                    meta.agent_alias = sidecar.agent_alias;
                    meta.name = sidecar.name;
                }
                meta
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

    /// Quick existence probe. A session exists if either the JSONL data file
    /// or the sidecar metadata file is present on disk. This prevents
    /// meta-only sessions (handshake with no messages) from becoming
    /// invisible tombstones.
    fn session_exists(&self, session_key: &str) -> bool {
        self.session_path(session_key).exists() || self.meta_path(session_key).exists()
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        let mut meta = self.read_meta(session_key)?;
        meta.agent_alias = Some(agent_alias.to_string());
        self.write_meta(session_key, &meta)
    }

    fn claim_session_agent_alias(
        &self,
        session_key: &str,
        agent_alias: &str,
    ) -> std::io::Result<crate::session_backend::ClaimOutcome> {
        use crate::session_backend::ClaimOutcome;
        use std::fs::OpenOptions;

        // Per-instance mutex as fast path for same-process threads.
        let _guard = self
            .claim_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Per-session lock file for cross-process serialization.
        let lock_path = self
            .sessions_dir
            .join(format!("{}.claim.lock", sanitize_session_key(session_key)));
        let lock_file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(false)
            .open(&lock_path)?;
        fs4::fs_std::FileExt::lock_exclusive(&lock_file)?;

        let mut meta = self.read_meta(session_key)?;
        match meta.agent_alias {
            Some(ref existing) if existing != agent_alias => {
                Ok(ClaimOutcome::Conflict(existing.clone()))
            }
            Some(_) => Ok(ClaimOutcome::Claimed),
            None => {
                meta.agent_alias = Some(agent_alias.to_string());
                self.write_meta(session_key, &meta)?;
                Ok(ClaimOutcome::Claimed)
            }
        }
        // lock_file drops here → OS releases advisory lock
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        match std::fs::read_to_string(self.meta_path(session_key)) {
            Ok(json) => {
                let meta: SessionMeta = serde_json::from_str(&json)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                Ok(meta.agent_alias)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    // ── agent_alias sidecar (.meta.json) tests ──────────────────────

    #[test]
    fn agent_alias_roundtrip_through_meta_json() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend.set_session_agent_alias("gw_test", "sales").unwrap();
        assert_eq!(
            backend.get_session_agent_alias("gw_test").unwrap(),
            Some("sales".to_string())
        );
    }

    #[test]
    fn agent_alias_missing_meta_returns_none() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        assert_eq!(
            backend.get_session_agent_alias("no_such_session").unwrap(),
            None
        );
    }

    #[test]
    fn agent_alias_corrupt_meta_returns_error() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        // Write garbage to the meta.json sidecar
        std::fs::write(store.meta_path("gw_corrupt"), b"not valid json {{{").unwrap();

        assert!(store.get_session_agent_alias("gw_corrupt").is_err());
    }

    #[test]
    fn delete_session_cleans_up_meta_json() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        backend
            .append("gw_cleanup", &ChatMessage::user("hello"))
            .unwrap();
        backend
            .set_session_agent_alias("gw_cleanup", "sales")
            .unwrap();
        assert!(store.meta_path("gw_cleanup").exists());

        backend.delete_session("gw_cleanup").unwrap();
        // After delete, .meta.json should also be gone
        assert!(!store.meta_path("gw_cleanup").exists());
    }

    #[test]
    fn list_sessions_with_metadata_includes_agent_alias_from_sidecar() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let backend: &dyn SessionBackend = &store;

        // Create a gateway session and stamp its alias
        backend
            .append("gw_test_visible", &ChatMessage::user("hello"))
            .unwrap();
        backend
            .set_session_agent_alias("gw_test_visible", "sales")
            .unwrap();

        // The session must appear in the listing with its alias
        let listed = backend.list_sessions_with_metadata();
        let row = listed
            .iter()
            .find(|m| m.key == "gw_test_visible")
            .expect("gateway session should appear in metadata listing");
        assert_eq!(row.agent_alias.as_deref(), Some("sales"));
    }

    /// Verify that a PermissionDenied error on the meta-file removal path
    /// is propagated to the caller (not silently swallowed).  This is the
    /// meta-only variant: no .jsonl exists, so the jsonl removal is
    /// skipped and the meta removal error-handling code is exercised.
    #[test]
    #[cfg(unix)]
    fn delete_session_meta_only_permission_denied_returns_err() {
        use std::io::ErrorKind;

        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "meta_only_perm";

        // Create only a meta sidecar (no jsonl messages)
        store.set_session_agent_alias(key, "test_agent").unwrap();
        assert!(store.meta_path(key).exists());
        assert!(!store.session_path(key).exists());

        // Make the sessions directory non-writable
        let sessions_dir = tmp.path().join("sessions");
        let mut dir_perms = std::fs::metadata(&sessions_dir).unwrap().permissions();
        dir_perms.set_readonly(true);
        std::fs::set_permissions(&sessions_dir, dir_perms).unwrap();

        let result = store.delete_session(key);

        // Restore writability so TempDir cleanup can remove the directory.
        #[allow(clippy::permissions_set_readonly_false)]
        {
            let mut dir_perms = std::fs::metadata(&sessions_dir).unwrap().permissions();
            dir_perms.set_readonly(false);
            std::fs::set_permissions(&sessions_dir, dir_perms).unwrap();
        }

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::PermissionDenied);
        // Confirm the meta file still exists (deletion failed — not silently swallowed)
        assert!(store.meta_path(key).exists());
    }

    /// End-to-end flow for meta-only session → delete → key reuse.
    /// A WebSocket handshake that writes .meta.json but never sends a
    /// message must be fully cleanable so the same session key can be
    /// reused for a fresh session.
    #[test]
    fn delete_session_meta_only_then_reuse_key() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "meta_only_reuse";

        // Create only a meta sidecar (no jsonl messages)
        store.set_session_agent_alias(key, "test_agent").unwrap();
        assert!(store.meta_path(key).exists());
        assert!(!store.session_path(key).exists());

        // Delete the meta-only session
        let deleted = store.delete_session(key).unwrap();
        assert!(deleted);
        assert!(!store.meta_path(key).exists());

        // Reuse the same key — should work as a fresh session
        store
            .append(key, &ChatMessage::user("hello after reuse"))
            .unwrap();
        let messages = store.load(key);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "hello after reuse");
        assert!(store.session_path(key).exists());
    }

    #[test]
    fn claim_ownership_first_caller_wins_then_conflict() {
        use crate::session_backend::ClaimOutcome;
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();
        let key = "claim_race";

        // First claim on an unowned session succeeds.
        assert_eq!(
            store.claim_session_agent_alias(key, "alice").unwrap(),
            ClaimOutcome::Claimed
        );
        // Same alias re-claiming is idempotent.
        assert_eq!(
            store.claim_session_agent_alias(key, "alice").unwrap(),
            ClaimOutcome::Claimed
        );
        // A different alias is rejected with the existing owner reported.
        assert_eq!(
            store.claim_session_agent_alias(key, "bob").unwrap(),
            ClaimOutcome::Conflict("alice".to_string())
        );
        // The stored owner is unchanged.
        assert_eq!(
            store.get_session_agent_alias(key).unwrap(),
            Some("alice".to_string())
        );
    }

    #[test]
    fn claim_ownership_concurrent_only_one_wins() {
        use crate::session_backend::ClaimOutcome;
        use std::sync::Arc;
        let tmp = TempDir::new().unwrap();
        let store = Arc::new(SessionStore::new(tmp.path()).unwrap());
        let key = "concurrent_claim";

        let mut handles = Vec::new();
        for i in 0..8 {
            let store = Arc::clone(&store);
            let alias = format!("agent{i}");
            handles.push(std::thread::spawn(move || {
                store.claim_session_agent_alias(key, &alias).unwrap()
            }));
        }
        let outcomes: Vec<ClaimOutcome> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let claimed = outcomes
            .iter()
            .filter(|o| matches!(o, ClaimOutcome::Claimed))
            .count();
        // Exactly one alias may claim an unowned session; the rest conflict.
        assert_eq!(claimed, 1, "exactly one concurrent claim must win");
        assert_eq!(outcomes.len(), 8);
    }

    #[test]
    fn claim_cross_instance_only_one_wins() {
        use crate::session_backend::ClaimOutcome;
        let tmp = TempDir::new().unwrap();
        let store_a = SessionStore::new(tmp.path()).unwrap();
        let store_b = SessionStore::new(tmp.path()).unwrap();

        // Pre-create the session with a message so it's non-empty.
        store_a
            .append("gw_test", &ChatMessage::user("hello"))
            .unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let b1 = barrier.clone();
        let jh_a = std::thread::spawn(move || {
            b1.wait();
            store_a
                .claim_session_agent_alias("gw_test", "alice")
                .unwrap()
        });
        let jh_b = std::thread::spawn(move || {
            barrier.wait();
            store_b.claim_session_agent_alias("gw_test", "bob").unwrap()
        });

        let ra = jh_a.join().unwrap();
        let rb = jh_b.join().unwrap();

        // Exactly one must be Claimed, the other Conflict.
        let claimed =
            matches!(ra, ClaimOutcome::Claimed) as u8 + matches!(rb, ClaimOutcome::Claimed) as u8;
        assert_eq!(claimed, 1, "only one caller should win");
    }
}
