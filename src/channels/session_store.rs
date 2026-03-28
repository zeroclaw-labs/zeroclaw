//! JSONL-based session persistence for channel conversations.
//!
//! Each session (keyed by `channel_sender` or `channel_thread_sender`) is stored
//! as an append-only JSONL file in `{workspace}/sessions/`. Messages are appended
//! one-per-line as JSON, never modifying old lines. On daemon restart, sessions
//! are loaded from disk to restore conversation context.

use crate::channels::session_backend::SessionBackend;
use crate::providers::traits::ChatMessage;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Append-only JSONL session store for channel conversations.
pub struct SessionStore {
    sessions_dir: PathBuf,
}

impl SessionStore {
    /// Create a new session store, ensuring the sessions directory exists.
    pub fn new(workspace_dir: &Path) -> std::io::Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)?;
        Ok(Self { sessions_dir })
    }

    /// Compute the file path for a session key using reversible percent-encoding.
    /// Characters that are not alphanumeric, `_`, or `-` are encoded as `%XX`.
    /// This ensures the key can be recovered exactly from the filename.
    fn session_path(&self, session_key: &str) -> PathBuf {
        let safe_key: String = session_key
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    format!("{c}")
                } else {
                    format!("%{:02X}", c as u32)
                }
            })
            .collect();
        self.sessions_dir.join(format!("{safe_key}.jsonl"))
    }

    /// Legacy file path using the old lossy sanitization (pre-#4806).
    /// Used as a fallback when loading sessions persisted before the encoding fix.
    fn legacy_session_path(&self, session_key: &str) -> PathBuf {
        let safe_key: String = session_key
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.sessions_dir.join(format!("{safe_key}.jsonl"))
    }

    /// Load all messages for a session from its JSONL file.
    /// Returns an empty vec if the file does not exist or is unreadable.
    /// Falls back to legacy (lossy-sanitized) filename if the encoded path doesn't exist.
    pub fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let path = self.session_path(session_key);
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => {
                // Fallback: try legacy path for sessions persisted before #4806
                let legacy_path = self.legacy_session_path(session_key);
                match std::fs::File::open(&legacy_path) {
                    Ok(f) => f,
                    Err(_) => return Vec::new(),
                }
            }
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

    /// Migrate a legacy-encoded session file to the new percent-encoded path.
    fn maybe_migrate_legacy_file(&self, session_key: &str) {
        let new_path = self.session_path(session_key);
        if new_path.exists() {
            return;
        }
        let legacy_path = self.legacy_session_path(session_key);
        if legacy_path.exists() {
            let _ = std::fs::rename(&legacy_path, &new_path);
        }
    }

    /// Append a single message to the session JSONL file.
    /// Migrates legacy-encoded files to the new encoding on first append.
    pub fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        self.maybe_migrate_legacy_file(session_key);
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
    ///
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

    /// Delete a session's JSONL file. Returns `true` if the file existed.
    /// Also removes legacy-encoded file if present.
    pub fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let path = self.session_path(session_key);
        if path.exists() {
            std::fs::remove_file(&path)?;
            return Ok(true);
        }
        // Try legacy path
        let legacy_path = self.legacy_session_path(session_key);
        if legacy_path.exists() {
            std::fs::remove_file(&legacy_path)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Return the modification time of a session's JSONL file.
    pub fn session_mtime(&self, session_key: &str) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.session_path(session_key))
            .and_then(|m| m.modified())
            .ok()
    }

    /// List all session keys that have files on disk.
    /// Decodes percent-encoded filenames back to original keys.
    pub fn list_sessions(&self) -> Vec<String> {
        let entries = match std::fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().into_string().ok()?;
                let encoded_key = name.strip_suffix(".jsonl")?;
                Some(decode_session_key(encoded_key))
            })
            .collect()
    }
}

/// Decode a percent-encoded session key back to the original string.
/// `%XX` sequences are decoded to the corresponding character.
fn decode_session_key(encoded: &str) -> String {
    let mut result = String::new();
    let mut chars = encoded.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(code) = u32::from_str_radix(&hex, 16) {
                    if let Some(decoded) = char::from_u32(code) {
                        result.push(decoded);
                        continue;
                    }
                }
            }
            // Malformed escape — pass through as-is
            result.push('%');
            result.push_str(&hex);
        } else {
            result.push(c);
        }
    }
    result
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

    fn compact(&self, session_key: &str) -> std::io::Result<()> {
        self.compact(session_key)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        self.delete_session(session_key)
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
    fn key_encoding_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        // Keys with special chars should survive encode → decode roundtrip
        let key = "matrix_@user:m.org||!room:m.org_$evt123:m.org_@user:m.org";
        store.append(key, &ChatMessage::user("test")).unwrap();

        let messages = store.load(key);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "test");

        // list_sessions should return the original key
        let sessions = store.list_sessions();
        assert!(sessions.contains(&key.to_string()));
    }

    #[test]
    fn key_encoding_special_chars() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        // All special chars in Matrix keys should survive
        let key = "slack/thread:123/user";
        store.append(key, &ChatMessage::user("test")).unwrap();

        let messages = store.load(key);
        assert_eq!(messages.len(), 1);

        let sessions = store.list_sessions();
        assert!(sessions.contains(&key.to_string()));
    }

    #[test]
    fn legacy_file_fallback() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        // Simulate a legacy file written with lossy sanitization
        let key = "matrix_@user:m.org||!room:m.org";
        let legacy_path = store.legacy_session_path(key);
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&legacy_path).unwrap();
        writeln!(file, r#"{{"role":"user","content":"legacy msg"}}"#).unwrap();

        // load() should find it via legacy fallback
        let messages = store.load(key);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "legacy msg");
    }

    #[test]
    fn legacy_file_migration_on_append() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path()).unwrap();

        // Create a legacy file
        let key = "matrix_@user:m.org||!room:m.org";
        let legacy_path = store.legacy_session_path(key);
        std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&legacy_path).unwrap();
        writeln!(file, r#"{{"role":"user","content":"old msg"}}"#).unwrap();

        // Append should migrate the file
        store
            .append(key, &ChatMessage::assistant("new msg"))
            .unwrap();

        // Legacy file should be gone, new-encoded file should exist
        assert!(!legacy_path.exists());
        assert!(store.session_path(key).exists());

        // All messages should be present
        let messages = store.load(key);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "old msg");
        assert_eq!(messages[1].content, "new msg");
    }

    #[test]
    fn decode_session_key_roundtrip() {
        let keys = vec![
            "matrix_@user:m.org||!room:m.org_$evt:m.org_@user:m.org",
            "simple_key_no_specials",
            "slack/thread:123/user",
            "discord_12345_67890",
            "email_user@example.com",
        ];
        for key in keys {
            let store = SessionStore {
                sessions_dir: PathBuf::from("/tmp"),
            };
            let path = store.session_path(key);
            let filename = path.file_stem().unwrap().to_str().unwrap();
            let decoded = super::decode_session_key(filename);
            assert_eq!(decoded, key, "roundtrip failed for key: {key}");
        }
    }

    #[test]
    fn list_sessions_returns_decoded_keys() {
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
}
