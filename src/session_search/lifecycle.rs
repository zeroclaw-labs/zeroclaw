//! Session lifecycle hooks — convenience wrappers for agent-loop integration.
//!
//! Callers (agent loop, channels) invoke these hooks at the appropriate points
//! to populate chat_sessions / chat_messages for cross-session recall.

use super::SessionSearchStore;
use anyhow::Result;
use std::sync::Arc;

/// Session handle — tracks the current session for message appends.
#[derive(Clone)]
pub struct SessionHandle {
    store: Arc<SessionSearchStore>,
    session_id: String,
}

impl SessionHandle {
    /// Start a new session and return a handle for appending messages.
    pub fn start(
        store: Arc<SessionSearchStore>,
        platform: Option<&str>,
        category: Option<&str>,
        title: Option<&str>,
    ) -> Result<Self> {
        let session_id = uuid::Uuid::new_v4().to_string();
        store.create_session(&session_id, platform, category, title)?;
        Ok(Self { store, session_id })
    }

    /// Resume an existing session by id.
    pub fn resume(store: Arc<SessionSearchStore>, session_id: &str) -> Self {
        Self {
            store,
            session_id: session_id.to_string(),
        }
    }

    pub fn id(&self) -> &str {
        &self.session_id
    }

    /// Append a user message to the session transcript.
    pub fn record_user_message(&self, content: &str) -> Result<()> {
        self.store
            .append_message(&self.session_id, "user", content)
            .map(|_| ())
    }

    /// Append an assistant message to the session transcript.
    pub fn record_assistant_message(&self, content: &str) -> Result<()> {
        self.store
            .append_message(&self.session_id, "assistant", content)
            .map(|_| ())
    }

    /// Append a tool-execution result to the transcript.
    pub fn record_tool_message(&self, content: &str) -> Result<()> {
        self.store
            .append_message(&self.session_id, "tool", content)
            .map(|_| ())
    }

    /// Mark the session as ended.
    pub fn end(&self) -> Result<()> {
        self.store.end_session(&self.session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::Connection;

    fn test_store() -> Arc<SessionSearchStore> {
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&conn).unwrap();
        Arc::new(SessionSearchStore::new(
            Arc::new(Mutex::new(conn)),
            "test".into(),
        ))
    }

    #[test]
    fn handle_records_messages() {
        let store = test_store();
        let handle = SessionHandle::start(store.clone(), Some("app"), Some("coding"), None).unwrap();
        handle.record_user_message("Hello").unwrap();
        handle.record_assistant_message("Hi").unwrap();
        handle.end().unwrap();

        let msgs = store.get_session_messages(handle.id()).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
    }
}
