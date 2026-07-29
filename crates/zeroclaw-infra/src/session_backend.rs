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

/// Outcome of an atomic ownership claim (`claim_session_agent_alias`).
///
/// The claim is a compare-and-set performed inside the backend's own lock so
/// that concurrent callers cannot both believe they own the session. This is
/// the fail-closed invariant used before any history load across HTTP, RPC,
/// and the WebSocket handshake — replacing the previous "get then set" pattern
/// where a caller could overwrite the owner between the read and the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// The session had no recorded owner and this alias is now recorded as
    /// the owner (or the session already belonged to this alias).
    Claimed,
    /// The session is owned by a different alias; the caller must not load
    /// history. Carries the existing owner for the error message.
    Conflict(String),
}

/// Trait for session persistence backends.
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
    /// handshake and HTTP chat-completions session-load when the alias is known.
    ///
    /// # Compatibility note for custom backends
    ///
    /// The default implementation returns `Unsupported`, which causes the
    /// gateway handlers to:
    ///
    /// - **Empty sessions (new):** Silently accept the turn. No ownership
    ///   enforcement applies -- any agent alias can use this session key, and
    ///   cross-agent isolation is absent.
    /// - **Non-empty sessions (existing data):** The handler returns HTTP 400
    ///   (`"Cannot resume session: backend does not track agent ownership"`).
    ///   Sessions with prior data become **unusable** until the backend
    ///   implements this method.
    ///
    /// # Data model contract
    ///
    /// `agent_alias` is a string that identifies the owning agent. It
    /// corresponds to the key in `config.agents` (the `[agents.<alias>]`
    /// TOML section name). The alias is written once on first access and
    /// not changed thereafter (except via `clear_agent_attribution` /
    /// `rename_agent_attribution`).
    ///
    /// # Migration path for existing backends
    ///
    /// Backends with session data from before these methods were introduced
    /// must backfill `agent_alias` metadata. Returning `Ok(None)` will NOT
    /// allow the first caller to claim ownership of a non-empty session
    /// (the handler returns HTTP 400).
    ///
    /// Single-agent deployments may implement this as a no-op returning
    /// `Ok(())` to avoid the 400 rejection.
    fn set_session_agent_alias(
        &self,
        _session_key: &str,
        _agent_alias: &str,
    ) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "session agent alias tracking not supported by this backend; cross-agent session isolation is unavailable",
        ))
    }

    /// Atomically claim ownership of a session for `agent_alias`.
    ///
    /// Unlike `set_session_agent_alias` (an idempotent setter that
    /// unconditionally overwrites), this is a compare-and-set: it records the
    /// alias only if the session has no owner, and reports a conflict if a
    /// *different* alias already owns it. Backends must perform the read and
    /// the conditional write under the same lock so two concurrent callers
    /// cannot both observe "no owner" and both write.
    ///
    /// Callers use this before loading history so a caller-controlled session
    /// key cannot reassign ownership and then expose another agent's
    /// transcript.
    ///
    /// # Return values
    /// - `Ok(ClaimOutcome::Claimed)` — no prior owner (now `agent_alias`) or
    ///   the session already belonged to `agent_alias`.
    /// - `Ok(ClaimOutcome::Conflict(existing))` — owned by a different alias.
    /// - `Err(Unsupported)` — backend does not track ownership. Handlers treat
    ///   this the same way they treat `set_session_agent_alias` returning
    ///   `Unsupported` (graceful degradation: empty sessions accepted,
    ///   non-empty sessions rejected).
    ///
    /// Returns `Err(Unsupported)` by default. Third-party backends that want
    /// cross-agent session isolation **must** override this method with an
    /// atomic compare-and-set (JSONL: claim-lock-guarded read+write;
    /// SQLite: INSERT … ON CONFLICT DO UPDATE … WHERE agent_alias IS NULL).
    ///
    /// Returning `Unsupported` tells the handler the backend cannot enforce
    /// ownership. HTTP and RPC handlers accept empty sessions when `Unsupported` is
    /// returned as graceful degradation (request-scoped transports can
    /// safely admit one empty turn).  WebSocket rejects `Unsupported`
    /// unconditionally because connection-scoped transports require
    /// deterministic ownership recording to prevent multi-agent admission.
    fn claim_session_agent_alias(
        &self,
        _session_key: &str,
        _agent_alias: &str,
    ) -> std::io::Result<ClaimOutcome> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "claim_session_agent_alias not supported by this backend; \
             implement atomic compare-and-set for cross-agent session isolation",
        ))
    }

    /// Get the agent alias associated with a session, if recorded.
    ///
    /// # Compatibility note for custom backends
    ///
    /// The default implementation returns `Unsupported`, which causes the
    /// gateway handlers to:
    ///
    /// - **Empty sessions (new):** Silently accept the turn. No ownership
    ///   enforcement applies.
    /// - **Non-empty sessions (existing data):** The handler returns HTTP 400
    ///   (`"Cannot resume session: backend does not track agent ownership"`).
    ///   Sessions with prior data become **unusable** until the backend
    ///   implements this method.
    ///
    /// # Data model contract
    ///
    /// Returns `Ok(Some(alias))` if the session has a recorded owner,
    /// `Ok(None)` if the session exists but has no recorded owner, or
    /// `Err(Unsupported)` if the backend lacks ownership tracking.
    /// Returning `Ok(None)` tells the handler the session has no recorded
    /// owner, which is accepted for **empty** sessions but rejected for
    /// **non-empty** sessions (to prevent cross-agent history leakage).
    ///
    /// # Migration path for existing backends
    ///
    /// Backends with pre-existing session data must backfill `agent_alias`
    /// metadata. Returning `Ok(None)` for a non-empty session is rejected
    /// (HTTP 400) -- backfill is the only supported migration path.
    ///
    /// For single-agent deployments, return `Ok(Some(agent_alias))` to
    /// avoid the 400 rejection. `Ok(None)` will be rejected for non-empty
    /// sessions.
    fn get_session_agent_alias(&self, _session_key: &str) -> std::io::Result<Option<String>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "session agent alias tracking not supported by this backend; cross-agent session isolation is unavailable",
        ))
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

    /// The default `claim_session_agent_alias` implementation returns
    /// `Err(Unsupported)` so handlers can detect backends that do not
    /// implement atomic ownership tracking.
    #[test]
    fn default_claim_returns_unsupported() {
        struct MinimalBackend;
        impl SessionBackend for MinimalBackend {
            fn load(&self, _session_key: &str) -> Vec<ChatMessage> {
                Vec::new()
            }
            fn append(&self, _session_key: &str, _message: &ChatMessage) -> std::io::Result<()> {
                Ok(())
            }
            fn remove_last(&self, _session_key: &str) -> std::io::Result<bool> {
                Ok(false)
            }
            fn list_sessions(&self) -> Vec<String> {
                Vec::new()
            }
        }

        let result = MinimalBackend.claim_session_agent_alias("test", "alice");
        let err = result.expect_err("default claim_session_agent_alias must return Err");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::Unsupported,
            "error kind must be Unsupported"
        );
        assert!(
            err.to_string().contains("not supported"),
            "error message must contain 'not supported': {}",
            err
        );
    }
}
