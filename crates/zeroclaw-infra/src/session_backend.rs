//! Trait abstraction for session persistence backends.
//!
//! Backends store per-sender conversation histories. The trait is intentionally
//! minimal — load, append, remove_last, clear_messages, list — so that JSONL
//! and SQLite (and future backends) share a common interface.

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
///
/// Implementations must be `Send + Sync` for sharing across async tasks.
pub trait SessionBackend: Send + Sync {
    /// Load all messages for a session. Returns empty vec if session doesn't exist.
    fn load(&self, session_key: &str) -> Vec<ChatMessage>;

    /// Same as `load`, but each row carries its persisted `created_at`
    /// when the backend has one. Default impl falls back to `load`
    /// without timestamps so non-SQLite backends keep working.
    fn load_with_timestamps(&self, session_key: &str) -> Vec<TimestampedMessage> {
        self.load(session_key)
            .into_iter()
            .map(|message| TimestampedMessage {
                message,
                created_at: None,
            })
            .collect()
    }

    /// Append a single message to a session.
    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()>;

    /// Remove the last message from a session. Returns `true` if a message was removed.
    fn remove_last(&self, session_key: &str) -> std::io::Result<bool>;

    /// Update the content of the last message in a session. Used for incremental
    /// persistence of streaming responses — append a placeholder first, then
    /// update_last periodically as more content arrives. Returns `false` if
    /// the session is empty. Default implementation is remove_last + append
    /// (backends can override for efficiency).
    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        if self.remove_last(session_key)? {
            self.append(session_key, message)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

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
                    agent_alias: None,
                    channel_id: None,
                    room_id: None,
                    sender_id: None,
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

    /// Clear all messages from a session, keeping the session itself alive.
    /// Returns the number of messages removed.
    ///
    /// Override for production use. The default is O(n²) via iterative
    /// `remove_last` — acceptable for tests but may cause latency on
    /// sessions with >100 messages.
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

    /// Clear the agent attribution (`agent_alias` → NULL) on every session
    /// owned by `agent_alias`, returning the number of rows updated. Used by the
    /// agent-deletion cascade (#7175): the conversation history (a possibly
    /// channel-shared session) is kept, only the stale agent attribution is
    /// dropped. No-op for backends without per-agent metadata.
    fn clear_agent_attribution(&self, _agent_alias: &str) -> std::io::Result<usize> {
        Ok(0)
    }

    /// Re-point session attribution (`agent_alias` `from` → `to`) on every
    /// session owned by `from`, returning the number of rows updated. Used by
    /// the agent-rename cascade (#7468): the conversation history is kept and
    /// its attribution follows the renamed agent (contrast
    /// [`Self::clear_agent_attribution`], which drops it on delete). No-op for
    /// backends without per-agent metadata.
    fn rename_agent_attribution(&self, _from: &str, _to: &str) -> std::io::Result<usize> {
        Ok(0)
    }

    /// Read-only residue probe for the agent-rename cascade (#7940): the count of
    /// session-metadata rows [`Self::rename_agent_attribution`] WOULD re-point for
    /// `agent_alias`, without mutating anything. The gateway uses this to tell a
    /// genuine post-persist partial failure (attribution still lagging at the old
    /// alias) apart from an unrelated request, so a resume only fires on real
    /// residue. No-op default (0) for backends without per-agent metadata.
    fn count_agent_attribution(&self, _agent_alias: &str) -> std::io::Result<usize> {
        Ok(0)
    }

    /// Quick existence check used by the gateway to avoid resurrecting a
    /// session that the user just deleted (#7126). The default impl falls
    /// back to `get_session_metadata`; production backends should override
    /// with the cheapest query consistent with how `delete_session` decides
    /// what to wipe (SQLite: a `SELECT 1` against `session_metadata`).
    fn session_exists(&self, session_key: &str) -> bool {
        self.get_session_metadata(session_key).is_some()
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

    /// Record the channel / room / sender routing context for a session.
    /// Called by channel orchestrators right before the LLM dispatch so
    /// the session row can be filtered by platform attribute in the
    /// dashboard. No-op default; SQLite override fills the columns added
    /// in the structured-routing migration.
    fn set_session_context(
        &self,
        _session_key: &str,
        _context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        Ok(())
    }

    /// Look up metadata for a single session by key.
    ///
    /// The default impl loads all messages to derive the count and calls
    /// `get_session_name` for the name. `created_at` and `last_activity` are
    /// set to `Utc::now()` at call time — backends with stored timestamps
    /// (e.g. SQLite) should override this method.
    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        let messages = self.load(session_key);
        if messages.is_empty() {
            return None;
        }
        Some(SessionMetadata {
            key: session_key.to_string(),
            name: self.get_session_name(session_key).ok().flatten(),
            created_at: Utc::now(),
            last_activity: Utc::now(),
            message_count: messages.len(),
            agent_alias: None,
            channel_id: None,
            room_id: None,
            sender_id: None,
        })
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
