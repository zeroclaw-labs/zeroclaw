//! Disposable entrance DTOs. The canonical model is `session_events::catalog::Session`.
use chrono::{DateTime, Utc};
use zeroclaw_api::model_provider::ConversationMessage;

pub struct SessionTranscript {
    pub session_uuid: String,
    /// Owning principal (RFC 7141 session isolation). `None` marks an
    /// unscoped/legacy row, visible only to unscoped connections.
    pub principal_id: Option<String>,
    pub agent_alias: String,
    pub workspace_dir: String,
    pub interaction_surface: Option<String>,
    pub token_count: u64,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub messages: Vec<ConversationMessage>,
    /// Whether `messages`' first non-system entry is the synthetic
    /// history-trim breadcrumb. Rows written after the `trim_breadcrumb`
    /// column was added carry this as recorded by the owning turn loop,
    /// never inferred from text. A row from before that column existed has
    /// no recorded value (`NULL`); for that one-time legacy case only, it is
    /// inferred from the first non-system message's text, matching the
    /// interactive-session JSONL migration contract (see
    /// `zeroclaw_runtime::agent::history::load_interactive_session_history_with_crumb`).
    pub trim_breadcrumb: bool,
    /// Provider-facing retained context written by native RPC trims. `None`
    /// preserves the legacy provider-safe replay path.
    pub retained_context: Option<Vec<ConversationMessage>>,
}

pub enum SessionRestore {
    Missing,
    Killed,
    Restorable(Box<SessionTranscript>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKillTransition {
    Marked,
    AlreadyKilled,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOwnership {
    Owned,
    Foreign,
    Missing,
}

/// Lightweight summary for the ACP session picker. Avoids loading the full
/// message history just to render a one-line label per session.
pub struct SessionListEntry {
    pub session_uuid: String,
    /// Owning principal (RFC 7141). `None` = unscoped/legacy row.
    pub principal_id: Option<String>,
    pub agent_alias: String,
    pub workspace_dir: String,
    pub token_count: u64,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_count: usize,
}

#[derive(Debug)]
pub struct SessionMessagePage {
    /// Owner read in the same transaction as the page, for RPC authorization
    /// revalidation without loading the complete transcript.
    pub principal_id: Option<String>,
    pub messages: Vec<ConversationMessage>,
    pub next_cursor: Option<String>,
    pub has_older: bool,
}
