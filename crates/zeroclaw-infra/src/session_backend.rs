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
    /// Cross-turn conversation identity. This is a FACT of record creation,
    /// generated server-side and persisted by the backend (NOT computed from
    /// `session_key` - the key only LOCATES the record). `None` for legacy
    /// rows that predate the column / sidecar and have not yet been resolved.
    /// In durable mode the backend record is the single source of truth.
    pub conversation_id: Option<String>,
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

/// A channel conversation and its persisted identity.
#[derive(Debug, Clone)]
pub struct ChannelConversationRecord {
    pub conversation_id: String,
    pub history: Vec<ChatMessage>,
}

impl PartialEq for ChannelConversationRecord {
    fn eq(&self, other: &Self) -> bool {
        self.conversation_id == other.conversation_id
            && self.history.len() == other.history.len()
            && self
                .history
                .iter()
                .zip(&other.history)
                .all(|(left, right)| left.role == right.role && left.content == right.content)
    }
}

impl Eq for ChannelConversationRecord {}

/// One atomic mutation of a channel conversation.
#[derive(Debug, Clone, Copy)]
pub enum SessionMutation<'a> {
    Append(&'a ChatMessage),
    RemoveLast {
        expected_role: &'a str,
        expected_content: &'a str,
    },
    UpdateLast(&'a ChatMessage),
}

/// Outcome of a conversation-id-fenced session write.
///
/// A history append / update / rollback / compaction is conditional on the
/// session record still carrying the conversation id the turn captured before
/// any async work began. The result distinguishes the three lifecycle states a
/// stale writer can observe, so a real storage error (`Err`) is never confused
/// with an expected lifecycle race:
///
/// - `Applied`: the record still carried the expected id; the mutation landed.
/// - `Stale`: the record still exists but its id was rotated (`/new`/`/clear`);
///   the caller's captured id no longer matches. An expected race, not retried,
///   and must not recreate a record.
/// - `Deleted`: the record was removed; same non-retry, non-recreate contract.
///
/// `bool` is intentionally avoided: `false` cannot tell stale from deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionalSessionWrite {
    Applied,
    Stale,
    Deleted,
}

/// Trait for session persistence backends.
/// Implementations must be `Send + Sync` for sharing across async tasks.
pub trait SessionBackend: Send + Sync {
    /// Load all messages for a session. Returns empty vec if session doesn't exist.
    fn load(&self, session_key: &str) -> Vec<ChatMessage>;

    /// Load all messages for a durable Channel conversation without hiding
    /// backend failures. Backends that do not support Channel conversation
    /// records retain source compatibility through the default implementation.
    fn load_fallible(&self, _session_key: &str) -> std::io::Result<Vec<ChatMessage>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "fallible channel history reads are unsupported",
        ))
    }

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
                    conversation_id: None,
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

    fn set_session_context(
        &self,
        _session_key: &str,
        _context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        Ok(())
    }

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
            conversation_id: None,
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

    /// Open the current conversation record, creating an empty UUID-v4 record
    /// when it is absent. Backends without channel-record support return
    /// `Unsupported` by default so third-party implementations remain source
    /// compatible.
    fn open_conversation(&self, _session_key: &str) -> std::io::Result<ChannelConversationRecord> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "channel conversation records are unsupported",
        ))
    }

    /// Apply one mutation only while the record carries the expected identity.
    fn mutate_conversation_if_current(
        &self,
        _session_key: &str,
        _expected_conversation_id: &str,
        _mutation: SessionMutation<'_>,
    ) -> std::io::Result<ConditionalSessionWrite> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "conditional channel conversation writes are unsupported",
        ))
    }

    /// Read the current conversation id for `session_key` WITHOUT creating a
    /// record on miss. Returns `None` when the record is absent. Implementations
    /// MUST be independent of `get_session_metadata` (whose trait default leaves
    /// `conversation_id` unset for backends like JSONL), otherwise durable
    /// `is_current`/`existing_record_for_test` checks silently drop every turn.
    fn current_conversation_id(&self, _session_key: &str) -> std::io::Result<Option<String>> {
        Ok(self
            .get_session_metadata(_session_key)
            .and_then(|metadata| metadata.conversation_id))
    }

    #[doc(hidden)]
    fn resolve_or_create_conversation_id(&self, _key: &str) -> std::io::Result<String> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "legacy conversation identity API is unsupported",
        ))
    }
    #[doc(hidden)]
    fn clear_and_rotate_conversation(&self, _key: &str) -> std::io::Result<String> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "legacy conversation rotation API is unsupported",
        ))
    }
    #[doc(hidden)]
    fn append_if_conversation_matches(
        &self,
        _key: &str,
        _id: &str,
        _message: &ChatMessage,
    ) -> std::io::Result<ConditionalSessionWrite> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "legacy conditional conversation API is unsupported",
        ))
    }
    #[doc(hidden)]
    fn remove_last_if_conversation_matches(
        &self,
        _key: &str,
        _id: &str,
    ) -> std::io::Result<ConditionalSessionWrite> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "legacy conditional conversation API is unsupported",
        ))
    }
    #[doc(hidden)]
    fn update_last_if_conversation_matches(
        &self,
        _key: &str,
        _id: &str,
        _message: &ChatMessage,
    ) -> std::io::Result<ConditionalSessionWrite> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "legacy conditional conversation API is unsupported",
        ))
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

/// Shared contract every `SessionBackend` conditional-write impl must
/// satisfy. Lives at module level (not in each backend's test module) so the
/// JSONL and SQLite backends are pinned to ONE identical semantics. Both call
/// this from their own test modules plus a backend-specific error test.
#[cfg(test)]
pub(crate) fn assert_channel_conversation_contract(backend: &dyn SessionBackend) {
    let key = "channel.main_room_alice";
    let opened = backend.open_conversation(key).unwrap();
    let parsed = uuid::Uuid::parse_str(&opened.conversation_id).unwrap();
    assert_eq!(parsed.get_version_num(), 4);
    assert!(opened.history.is_empty());

    let reopened = backend.open_conversation(key).unwrap();
    assert_eq!(reopened.conversation_id, opened.conversation_id);
    assert!(reopened.history.is_empty());

    let user = ChatMessage::user("before");
    assert_eq!(
        backend
            .mutate_conversation_if_current(
                key,
                &opened.conversation_id,
                SessionMutation::Append(&user),
            )
            .unwrap(),
        ConditionalSessionWrite::Applied,
    );
    let assistant = ChatMessage::assistant("draft");
    backend
        .mutate_conversation_if_current(
            key,
            &opened.conversation_id,
            SessionMutation::Append(&assistant),
        )
        .unwrap();
    backend
        .mutate_conversation_if_current(
            key,
            &opened.conversation_id,
            SessionMutation::RemoveLast {
                expected_role: "assistant",
                expected_content: "mismatch",
            },
        )
        .unwrap();
    assert_eq!(backend.open_conversation(key).unwrap().history.len(), 2);
    backend
        .mutate_conversation_if_current(
            key,
            &opened.conversation_id,
            SessionMutation::RemoveLast {
                expected_role: "assistant",
                expected_content: "draft",
            },
        )
        .unwrap();
    let updated = ChatMessage::user("updated");
    backend
        .mutate_conversation_if_current(
            key,
            &opened.conversation_id,
            SessionMutation::UpdateLast(&updated),
        )
        .unwrap();
    let record = backend.open_conversation(key).unwrap();
    assert_eq!(record.conversation_id, opened.conversation_id);
    assert_eq!(record.history.len(), 1);
    assert_eq!(record.history[0].content, "updated");

    let stale_id = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        backend
            .mutate_conversation_if_current(
                key,
                &stale_id,
                SessionMutation::Append(&ChatMessage::assistant("stale")),
            )
            .unwrap(),
        ConditionalSessionWrite::Stale,
    );
    backend.delete_session(key).unwrap();
    assert_eq!(
        backend
            .mutate_conversation_if_current(
                key,
                &opened.conversation_id,
                SessionMutation::RemoveLast {
                    expected_role: "user",
                    expected_content: "updated",
                },
            )
            .unwrap(),
        ConditionalSessionWrite::Deleted,
    );
    assert!(!backend.session_exists(key));

    let fresh = backend.open_conversation(key).unwrap();
    assert_ne!(fresh.conversation_id, opened.conversation_id);
    assert_eq!(
        backend
            .mutate_conversation_if_current(
                key,
                &opened.conversation_id,
                SessionMutation::Append(&ChatMessage::assistant("stale")),
            )
            .unwrap(),
        ConditionalSessionWrite::Stale,
    );
    assert_eq!(
        uuid::Uuid::parse_str(&fresh.conversation_id)
            .unwrap()
            .get_version_num(),
        4
    );
    assert!(fresh.history.is_empty());
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
            conversation_id: None,
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
