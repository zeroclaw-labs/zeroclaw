//! Trait abstraction for session persistence backends.

use chrono::{DateTime, Utc};
use zeroclaw_api::model_provider::ChatMessage;

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
    /// Alias of the agent that owned this session (HashMap key in
    /// `config.agents`). `None` for sessions persisted before per-agent
    /// attribution landed, or for backends that don't track it.
    pub agent_alias: Option<String>,
    /// Dotted ChannelRef the session belongs to (`<type>.<alias>`,
    /// e.g. `discord.clamps`). `None` for non-channel sessions (CLI,
    /// internal cron runs) or backends without routing columns.
    pub channel_id: Option<String>,
    /// Platform-side room / thread identifier (Discord channel id,
    /// Matrix room id, Slack thread ts, ...). `None` for direct messages
    /// or backends that don't track it.
    pub room_id: Option<String>,
    /// Inbound sender id verbatim (Discord username, phone number, ...).
    /// Not an FK — sessions can survive deletion of the upstream user.
    pub sender_id: Option<String>,
}

/// Structured routing context recorded alongside a session. Mirrors the
/// `ChannelMessage` fields the orchestrator uses to compose
/// `conversation_history_key` so the session row can be queried by
/// channel / room / sender without re-parsing the synthetic key.
#[derive(Debug, Clone, Default)]
pub struct SessionContext<'a> {
    /// `<type>.<alias>` ChannelRef (`discord.clamps`).
    pub channel_id: Option<&'a str>,
    /// Platform-side room / thread id.
    pub room_id: Option<&'a str>,
    /// Inbound sender id (channel-native username, phone, ...).
    pub sender_id: Option<&'a str>,
}

/// Query parameters for listing sessions.
#[derive(Debug, Clone, Default)]
pub struct SessionQuery {
    /// Keyword to search in session messages (FTS5 if available).
    pub keyword: Option<String>,
    /// Maximum number of sessions to return.
    pub limit: Option<usize>,
}

/// One persisted message with the optional `created_at` the backend
/// stamped on it. JSONL / in-memory backends return `None`; SQLite
/// returns the row's `created_at` column.
#[derive(Debug, Clone)]
pub struct TimestampedMessage {
    pub message: ChatMessage,
    pub created_at: Option<DateTime<Utc>>,
}

/// Trait for session persistence backends.
/// Implementations must be `Send + Sync` for sharing across async tasks.
pub trait SessionBackend: Send + Sync {
    /// Load all messages for a session. Returns an error if the backend fails.
    fn load(&self, session_key: &str) -> std::io::Result<Vec<ChatMessage>>;

    /// Same as `load`, but each row carries its persisted `created_at`
    /// when the backend has one.
    fn load_with_timestamps(&self, session_key: &str) -> std::io::Result<Vec<TimestampedMessage>> {
        self.load(session_key).map(|messages| {
            messages
                .into_iter()
                .map(|message| TimestampedMessage {
                    message,
                    created_at: None,
                })
                .collect()
        })
    }

    /// Append a single message to a session.
    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()>;

    /// Remove the last message from a session. Returns `true` if a message was removed.
    fn remove_last(&self, session_key: &str) -> std::io::Result<bool>;

    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        if self.remove_last(session_key)? {
            self.append(session_key, message)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// List all session keys. Returns an error if the backend fails.
    fn list_sessions(&self) -> std::io::Result<Vec<String>>;

    /// List sessions with metadata. Returns an error if the backend fails.
    ///
    /// # Default Implementation
    ///
    /// The default implementation returns `Err` to force backend authors to provide
    /// a batch-loaded implementation. The previous O(N) sequential call pattern
    /// would block the executor thread for an unacceptable duration with many sessions,
    /// potentially causing thread pool exhaustion.
    ///
    /// Implementations MUST override this method with a single batch query that fetches
    /// all metadata without per-session lookups. PostgreSQL and SQLite backends do this
    /// with a single `SELECT` from `session_metadata`.
    ///
    /// # Recursion Warning
    ///
    /// Implementations of `get_session_metadata` must NOT call `list_sessions_with_metadata`,
    /// as this will cause infinite recursion. The separation is enforced by requiring
    /// explicit batch implementations rather than providing a default that could be misused.
    fn list_sessions_with_metadata(&self) -> std::io::Result<Vec<SessionMetadata>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "list_sessions_with_metadata must be implemented by the backend with a batch query; \
             the default O(N) sequential implementation is disabled to prevent thread pool exhaustion",
        ))
    }

    /// Compact a session file (remove duplicates/corruption). No-op by default.
    fn compact(&self, _session_key: &str) -> std::io::Result<()> {
        Ok(())
    }

    /// Remove sessions that haven't been active within the given TTL hours.
    fn cleanup_stale(&self, _ttl_hours: u32) -> std::io::Result<usize> {
        Ok(0)
    }

    /// Search sessions by keyword. Returns an error if the backend fails.
    fn search(&self, _query: &SessionQuery) -> std::io::Result<Vec<SessionMetadata>> {
        Ok(Vec::new())
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let mut count = 0;
        while self.remove_last(session_key)? {
            count += 1;
        }
        Ok(count)
    }

    /// Delete all messages for a session. Returns `true` if the session existed.
    fn delete_session(&self, _session_key: &str) -> std::io::Result<bool> {
        Ok(false)
    }

    fn clear_agent_attribution(&self, _agent_alias: &str) -> std::io::Result<usize> {
        Ok(0)
    }

    fn rename_agent_attribution(&self, _from: &str, _to: &str) -> std::io::Result<usize> {
        Ok(0)
    }

    fn count_agent_attribution(&self, _agent_alias: &str) -> std::io::Result<usize> {
        Ok(0)
    }

    fn session_exists(&self, session_key: &str) -> std::io::Result<bool> {
        match self.get_session_metadata(session_key) {
            Ok(opt) => Ok(opt.is_some()),
            Err(e) => Err(e),
        }
    }

    /// Set or update the human-readable name for a session.
    fn set_session_name(&self, _session_key: &str, _name: &str) -> std::io::Result<()> {
        Ok(())
    }

    /// Get the human-readable name for a session (if set).
    fn get_session_name(&self, _session_key: &str) -> std::io::Result<Option<String>> {
        Ok(None)
    }

    /// Record the agent alias that owns a session. Called on WebSocket
    /// handshake when the alias is known. No-op for backends that don't
    /// track per-agent attribution.
    fn set_session_agent_alias(
        &self,
        _session_key: &str,
        _agent_alias: &str,
    ) -> std::io::Result<()> {
        Ok(())
    }

    /// Get the agent alias associated with a session, if recorded.
    fn get_session_agent_alias(&self, _session_key: &str) -> std::io::Result<Option<String>> {
        Ok(None)
    }

    fn set_session_context(
        &self,
        _session_key: &str,
        _context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        Ok(())
    }

    /// Get metadata for a specific session by key.
    ///
    /// # Performance Contract
    ///
    /// This method MUST complete in O(1) or O(log N) time. It is used for
    /// lightweight probes (existence checks, ownership validation, health
    /// probes) and must not block the executor thread.
    ///
    /// # Implementation Requirements
    ///
    /// - **DO NOT** call `self.load(session_key)` — loading all messages is O(N)
    ///   and violates the performance contract.
    /// - **DO** query only the metadata table (or equivalent) with a key-based
    ///   lookup. PostgreSQL and SQLite backends do this with a single `SELECT`
    ///   from `session_metadata WHERE session_key = $1`.
    ///
    /// The default implementation returns `Err(Unsupported)` to enforce that
    /// backend authors provide a proper O(1) implementation. The previous
    /// implementation that called `self.load()` was a recursion hazard and
    /// violated the performance contract.
    fn get_session_metadata(&self, _session_key: &str) -> std::io::Result<Option<SessionMetadata>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "get_session_metadata must be implemented by the backend with an O(1) \
             metadata-only query; the default implementation that calls self.load() \
             is disabled to prevent recursion and performance violations",
        ))
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

    /// List sessions currently in "running" state. Returns an error if the backend fails.
    fn list_running_sessions(&self) -> std::io::Result<Vec<SessionMetadata>> {
        Ok(Vec::new())
    }

    /// List sessions stuck in "running" state longer than `threshold_secs`.
    /// Returns an error if the backend fails.
    fn list_stuck_sessions(&self, _threshold_secs: u64) -> std::io::Result<Vec<SessionMetadata>> {
        Ok(Vec::new())
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
            agent_alias: None,
            channel_id: None,
            room_id: None,
            sender_id: None,
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
}
