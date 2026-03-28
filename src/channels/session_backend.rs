//! Trait abstraction for session persistence backends.
//!
//! Backends store per-sender conversation histories. The trait is intentionally
//! minimal — load, append, remove_last, list — so that JSONL and SQLite (and
//! future backends) share a common interface.

use crate::providers::traits::ChatMessage;
use chrono::{DateTime, Utc};

/// Metadata about a persisted session.
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    /// Session key (e.g. `telegram_user123`).
    pub key: String,
    /// Optional human-readable name (e.g. `eyrie-commander-briefing`).
    pub name: Option<String>,
    /// When the session was first created.
    pub created_at: DateTime<Utc>,
    /// When the last message was appended.
    pub last_activity: DateTime<Utc>,
    /// Total number of messages in the session.
    pub message_count: usize,
}

/// Query parameters for listing sessions.
#[derive(Debug, Clone, Default)]
pub struct SessionQuery {
    /// Keyword to search in session messages (FTS5 if available).
    pub keyword: Option<String>,
    /// Maximum number of sessions to return.
    pub limit: Option<usize>,
}

/// Trait for session persistence backends.
///
/// Implementations must be `Send + Sync` for sharing across async tasks.
pub trait SessionBackend: Send + Sync {
    /// Load all messages for a session. Returns empty vec if session doesn't exist.
    fn load(&self, session_key: &str) -> Vec<ChatMessage>;

    /// Append a single message to a session.
    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()>;

    /// Remove the last message from a session. Returns `true` if a message was removed.
    fn remove_last(&self, session_key: &str) -> std::io::Result<bool>;

    /// List all session keys.
    fn list_sessions(&self) -> Vec<String>;

    /// List sessions with metadata.
    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        // Default: construct metadata from messages (backends can override for efficiency)
        self.list_sessions()
            .into_iter()
            .map(|key| {
                let messages = self.load(&key);
                SessionMetadata {
                    key,
                    name: None,
                    created_at: Utc::now(),
                    last_activity: Utc::now(),
                    message_count: messages.len(),
                }
            })
            .collect()
    }

    /// Compact a session file (remove duplicates/corruption). No-op by default.
    fn compact(&self, _session_key: &str) -> std::io::Result<()> {
        Ok(())
    }

    /// Remove sessions that haven't been active within the given TTL hours.
    fn cleanup_stale(&self, _ttl_hours: u32) -> std::io::Result<usize> {
        Ok(0)
    }

    /// Search sessions by keyword. Default returns empty (backends with FTS override).
    fn search(&self, _query: &SessionQuery) -> Vec<SessionMetadata> {
        Vec::new()
    }

    /// Delete all messages for a session. Returns `true` if the session existed.
    fn delete_session(&self, _session_key: &str) -> std::io::Result<bool> {
        Ok(false)
    }

    /// Set or update the human-readable name for a session.
    fn set_session_name(&self, _session_key: &str, _name: &str) -> std::io::Result<()> {
        Ok(())
    }

    /// Get the human-readable name for a session (if set).
    fn get_session_name(&self, _session_key: &str) -> std::io::Result<Option<String>> {
        Ok(None)
    }

    /// Set the session state (e.g. "idle", "running", "error").
    /// `turn_id` identifies the current turn (set when running, cleared on idle).
    fn set_session_state(
        &self,
        _session_key: &str,
        _state: &str,
        _turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        Ok(())
    }

    /// Get the current session state. Returns `None` if the backend doesn't track state.
    fn get_session_state(&self, _session_key: &str) -> std::io::Result<Option<SessionState>> {
        Ok(None)
    }

    /// List sessions currently in "running" state.
    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        Vec::new()
    }

    /// List sessions stuck in "running" state longer than `threshold_secs`.
    fn list_stuck_sessions(&self, _threshold_secs: u64) -> Vec<SessionMetadata> {
        Vec::new()
    }
}

/// Session state information.
#[derive(Debug, Clone)]
pub struct SessionState {
    /// Current state: "idle", "running", or "error".
    pub state: String,
    /// Turn ID of the active or last turn.
    pub turn_id: Option<String>,
    /// When the current state was entered.
    pub turn_started_at: Option<DateTime<Utc>>,
}

/// A read-only session backend that merges listings from two backends.
///
/// Channel sessions live in JSONL files; gateway sessions live in SQLite.
/// This wrapper lets session tools see both. Primary wins on key conflicts;
/// writes delegate to primary only.
pub struct CompositeSessionBackend {
    primary: std::sync::Arc<dyn SessionBackend>,
    secondary: std::sync::Arc<dyn SessionBackend>,
}

impl CompositeSessionBackend {
    pub fn new(
        primary: std::sync::Arc<dyn SessionBackend>,
        secondary: std::sync::Arc<dyn SessionBackend>,
    ) -> Self {
        Self { primary, secondary }
    }
}

impl SessionBackend for CompositeSessionBackend {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let msgs = self.primary.load(session_key);
        if !msgs.is_empty() {
            return msgs;
        }
        self.secondary.load(session_key)
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        self.primary.append(session_key, message)
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        self.primary.remove_last(session_key)
    }

    fn list_sessions(&self) -> Vec<String> {
        let mut keys: std::collections::HashSet<String> =
            self.primary.list_sessions().into_iter().collect();
        keys.extend(self.secondary.list_sessions());
        keys.into_iter().collect()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        let primary_meta = self.primary.list_sessions_with_metadata();
        let mut seen: std::collections::HashSet<String> =
            primary_meta.iter().map(|m| m.key.clone()).collect();
        let mut result = primary_meta;
        for meta in self.secondary.list_sessions_with_metadata() {
            if seen.insert(meta.key.clone()) {
                result.push(meta);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_metadata_is_constructible() {
        let meta = SessionMetadata {
            key: "test".into(),
            name: None,
            created_at: Utc::now(),
            last_activity: Utc::now(),
            message_count: 5,
        };
        assert_eq!(meta.key, "test");
        assert_eq!(meta.message_count, 5);
    }

    #[test]
    fn session_query_defaults() {
        let q = SessionQuery::default();
        assert!(q.keyword.is_none());
        assert!(q.limit.is_none());
    }

    // ── CompositeSessionBackend tests ─────────────────────────────

    use crate::channels::session_sqlite::SqliteSessionBackend;
    use crate::channels::session_store::SessionStore;

    #[test]
    fn composite_merges_sessions_from_both_dirs() {
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();

        let store_a = SessionStore::new(dir_a.path()).unwrap();
        store_a
            .append("telegram__alice", &ChatMessage::user("hi"))
            .unwrap();

        let store_b = SessionStore::new(dir_b.path()).unwrap();
        store_b
            .append("gw_project-abc", &ChatMessage::user("hello"))
            .unwrap();

        let composite = CompositeSessionBackend::new(
            std::sync::Arc::new(store_a),
            std::sync::Arc::new(store_b),
        );
        let sessions = composite.list_sessions();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn composite_primary_wins_on_load_conflict() {
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();

        let store_a = SessionStore::new(dir_a.path()).unwrap();
        store_a
            .append("shared", &ChatMessage::user("primary"))
            .unwrap();

        let store_b = SessionStore::new(dir_b.path()).unwrap();
        store_b
            .append("shared", &ChatMessage::user("secondary"))
            .unwrap();

        let composite = CompositeSessionBackend::new(
            std::sync::Arc::new(store_a),
            std::sync::Arc::new(store_b),
        );
        let msgs = composite.load("shared");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "primary");

        // Deduplicated in listing
        assert_eq!(composite.list_sessions().len(), 1);
    }

    #[test]
    fn composite_falls_through_to_secondary() {
        let dir_a = tempfile::TempDir::new().unwrap();
        let dir_b = tempfile::TempDir::new().unwrap();

        let store_a = SessionStore::new(dir_a.path()).unwrap();
        let store_b = SessionStore::new(dir_b.path()).unwrap();
        store_b
            .append("gw_session", &ChatMessage::user("from gateway"))
            .unwrap();

        let composite = CompositeSessionBackend::new(
            std::sync::Arc::new(store_a),
            std::sync::Arc::new(store_b),
        );
        let msgs = composite.load("gw_session");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "from gateway");
    }

    #[test]
    fn composite_jsonl_plus_sqlite_with_overlapping_keys() {
        let dir = tempfile::TempDir::new().unwrap();

        // JSONL backend: channel session
        let jsonl = SessionStore::new(dir.path()).unwrap();
        jsonl
            .append("telegram__alice", &ChatMessage::user("channel msg"))
            .unwrap();
        // Overlapping key in JSONL
        jsonl
            .append("shared_session", &ChatMessage::user("jsonl version"))
            .unwrap();

        // SQLite backend: gateway sessions
        let sqlite = SqliteSessionBackend::new(dir.path()).unwrap();
        sqlite
            .append("gw_project-abc", &ChatMessage::user("gateway msg"))
            .unwrap();
        // Overlapping key in SQLite
        sqlite
            .append("shared_session", &ChatMessage::user("sqlite version"))
            .unwrap();

        let composite =
            CompositeSessionBackend::new(std::sync::Arc::new(jsonl), std::sync::Arc::new(sqlite));

        // Both unique sessions visible
        let sessions = composite.list_sessions();
        assert!(sessions.contains(&"telegram__alice".to_string()));
        assert!(sessions.contains(&"gw_project-abc".to_string()));
        assert!(sessions.contains(&"shared_session".to_string()));
        assert_eq!(sessions.len(), 3);

        // Primary (JSONL) wins on overlapping key
        let msgs = composite.load("shared_session");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "jsonl version");

        // Gateway-only session loads from secondary
        let gw_msgs = composite.load("gw_project-abc");
        assert_eq!(gw_msgs.len(), 1);
        assert_eq!(gw_msgs[0].content, "gateway msg");

        // Metadata listing also deduplicates
        let meta = composite.list_sessions_with_metadata();
        let keys: Vec<&str> = meta.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys.iter().filter(|k| **k == "shared_session").count(), 1);
    }
}
