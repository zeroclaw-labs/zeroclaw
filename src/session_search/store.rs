//! SessionSearchStore — FTS5 search over past conversation transcripts.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ChatSession {
    pub id: String,
    pub platform: Option<String>,
    pub category: Option<String>,
    pub title: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub device_id: String,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub timestamp: i64,
}

/// A single FTS5 hit, grouped later by session.
#[derive(Debug, Clone)]
pub struct RawHit {
    pub message_id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub timestamp: i64,
    pub rank: f64,
}

/// A session-level search result after grouping and snippet extraction.
#[derive(Debug, Clone)]
pub struct SessionSearchHit {
    pub session: ChatSession,
    /// Top matching message snippets from this session.
    pub snippets: Vec<String>,
    /// Aggregate relevance rank (higher = more relevant).
    pub rank: f64,
    /// Total number of matching messages in this session.
    pub match_count: usize,
}

pub struct SessionSearchStore {
    conn: Arc<Mutex<Connection>>,
    device_id: String,
}

impl SessionSearchStore {
    pub fn new(conn: Arc<Mutex<Connection>>, device_id: String) -> Self {
        Self { conn, device_id }
    }

    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock();
        super::schema::migrate(&conn).context("session search schema migration failed")
    }

    /// Create a new chat session.
    pub fn create_session(
        &self,
        id: &str,
        platform: Option<&str>,
        category: Option<&str>,
        title: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO chat_sessions (id, platform, category, title, device_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, platform, category, title, self.device_id],
        )?;
        Ok(())
    }

    /// End a session (record ended_at).
    pub fn end_session(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE chat_sessions SET ended_at = unixepoch() WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Append a message to a session's transcript.
    pub fn append_message(&self, session_id: &str, role: &str, content: &str) -> Result<i64> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO chat_messages (session_id, role, content) VALUES (?1, ?2, ?3)",
            params![session_id, role, content],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Search across all session transcripts using FTS5.
    ///
    /// Returns raw message hits; use `group_by_session` to aggregate.
    pub fn search_raw(&self, query: &str, limit: usize) -> Result<Vec<RawHit>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.role, m.content, m.timestamp, f.rank
             FROM chat_messages_fts f
             JOIN chat_messages m ON m.id = f.rowid
             WHERE chat_messages_fts MATCH ?1
             ORDER BY f.rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |row| {
            Ok(RawHit {
                message_id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                timestamp: row.get(4)?,
                rank: row.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("FTS5 session search failed")
    }

    /// Get session metadata by id.
    pub fn get_session(&self, id: &str) -> Result<Option<ChatSession>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, platform, category, title, started_at, ended_at, device_id
             FROM chat_sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(ChatSession {
                id: row.get(0)?,
                platform: row.get(1)?,
                category: row.get(2)?,
                title: row.get(3)?,
                started_at: row.get(4)?,
                ended_at: row.get(5)?,
                device_id: row.get(6)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    /// Get recent sessions (for listing, no search).
    pub fn recent_sessions(&self, limit: usize) -> Result<Vec<ChatSession>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, platform, category, title, started_at, ended_at, device_id
             FROM chat_sessions
             ORDER BY started_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(ChatSession {
                id: row.get(0)?,
                platform: row.get(1)?,
                category: row.get(2)?,
                title: row.get(3)?,
                started_at: row.get(4)?,
                ended_at: row.get(5)?,
                device_id: row.get(6)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to list recent sessions")
    }

    /// Search and group hits by session, returning top N sessions with snippets.
    pub fn search_sessions(&self, query: &str, limit_sessions: usize) -> Result<Vec<SessionSearchHit>> {
        // Fetch 3x raw hits to allow for grouping
        let raw = self.search_raw(query, limit_sessions * 5)?;

        // Group by session
        let mut grouped: HashMap<String, Vec<RawHit>> = HashMap::new();
        for hit in raw {
            grouped.entry(hit.session_id.clone()).or_default().push(hit);
        }

        let mut results: Vec<SessionSearchHit> = Vec::with_capacity(grouped.len());
        for (session_id, hits) in grouped {
            let Some(session) = self.get_session(&session_id)? else {
                continue;
            };

            // FTS5 rank is negative (more negative = better); invert for accumulation
            let total_rank = hits.iter().map(|h| -h.rank).sum::<f64>();
            let match_count = hits.len();
            let snippets: Vec<String> = hits.iter().take(3).map(|h| h.content.clone()).collect();

            results.push(SessionSearchHit {
                session,
                snippets,
                rank: total_rank,
                match_count,
            });
        }

        // Sort by aggregate rank (higher is better)
        results.sort_by(|a, b| b.rank.partial_cmp(&a.rank).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit_sessions);

        Ok(results)
    }

    /// Get all messages from a session (for full-context summarization).
    pub fn get_session_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, timestamp
             FROM chat_messages
             WHERE session_id = ?1
             ORDER BY timestamp ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(ChatMessage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                timestamp: row.get(4)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to get session messages")
    }

    /// Prune old sessions (retention policy).
    pub fn prune_older_than(&self, max_age_days: i64) -> Result<usize> {
        let conn = self.conn.lock();
        let cutoff = chrono::Utc::now().timestamp() - (max_age_days * 86400);
        let affected = conn.execute(
            "DELETE FROM chat_sessions WHERE started_at < ?1",
            params![cutoff],
        )?;
        Ok(affected)
    }
}

/// System prompt for summarizing session search hits.
pub const SESSION_SUMMARIZE_PROMPT: &str = r#"The user is searching their past conversations.
Given these matching conversation snippets, write a concise summary (2-3 sentences)
of what was discussed in each session and what conclusions were reached.
Include specific dates, decisions, and solutions when present.
Write in the same language as the snippets."#;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> SessionSearchStore {
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&conn).unwrap();
        SessionSearchStore::new(Arc::new(Mutex::new(conn)), "test-device".into())
    }

    #[test]
    fn create_and_retrieve_session() {
        let store = test_store();
        store.create_session("s1", Some("app"), Some("coding"), Some("Rust fix")).unwrap();
        let s = store.get_session("s1").unwrap().unwrap();
        assert_eq!(s.platform.as_deref(), Some("app"));
        assert_eq!(s.category.as_deref(), Some("coding"));
    }

    #[test]
    fn search_finds_matching_messages() {
        let store = test_store();
        store.create_session("s1", None, None, None).unwrap();
        store.create_session("s2", None, None, None).unwrap();

        store.append_message("s1", "user", "How do I fix the borrow checker issue?").unwrap();
        store.append_message("s1", "assistant", "Use Arc Mutex to share ownership").unwrap();
        store.append_message("s2", "user", "What's the weather today?").unwrap();

        let results = store.search_raw("borrow", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].session_id, "s1");
    }

    #[test]
    fn search_groups_by_session() {
        let store = test_store();
        store.create_session("s1", None, None, Some("Borrow fix")).unwrap();
        store.create_session("s2", None, None, Some("Weather chat")).unwrap();

        store.append_message("s1", "user", "borrow checker").unwrap();
        store.append_message("s1", "assistant", "borrow this way").unwrap();
        store.append_message("s2", "user", "borrow a book").unwrap();

        let hits = store.search_sessions("borrow", 5).unwrap();
        assert_eq!(hits.len(), 2);
        // s1 should rank higher (2 matches vs 1)
        assert_eq!(hits[0].session.id, "s1");
        assert_eq!(hits[0].match_count, 2);
    }

    #[test]
    fn get_session_messages_ordered() {
        let store = test_store();
        store.create_session("s1", None, None, None).unwrap();
        store.append_message("s1", "user", "first").unwrap();
        store.append_message("s1", "assistant", "second").unwrap();
        let msgs = store.get_session_messages("s1").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[1].content, "second");
    }
}
