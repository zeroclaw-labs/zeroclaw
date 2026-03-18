//! SQLite full-text store for memory content.
//!
//! Each memory is stored in a `memories` table (keyed by UUID) with an FTS5
//! virtual table for fast text search. A `vector_id` column links back to the
//! RVF store so we can hydrate content from vector search results.

use anyhow::Result;
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct StoredMemory {
    pub content: String,
    pub tags: String,
}

#[derive(Debug, Clone)]
pub struct StoredMemoryDetail {
    pub id: String,
    pub content: String,
    pub sender: String,
    pub channel: String,
    pub tags: String,
    pub kind: String,
    pub timestamp: i64,
}

pub struct TextStore {
    conn: Arc<Mutex<Connection>>,
}

impl TextStore {
    pub fn new(db_path: &str) -> Result<Self> {
        let expanded = shellexpand::tilde(db_path).to_string();
        if let Some(parent) = std::path::Path::new(&expanded).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&expanded)?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id        TEXT PRIMARY KEY,
                vector_id INTEGER NOT NULL,
                content   TEXT NOT NULL,
                sender    TEXT NOT NULL DEFAULT '',
                channel   TEXT NOT NULL DEFAULT '',
                tags      TEXT NOT NULL DEFAULT '',
                kind      TEXT NOT NULL DEFAULT 'chat',
                timestamp INTEGER NOT NULL DEFAULT 0
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content, tags, content='memories', content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, content, tags) VALUES (new.rowid, new.content, new.tags);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content, tags) VALUES('delete', old.rowid, old.content, old.tags);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content, tags) VALUES('delete', old.rowid, old.content, old.tags);
                INSERT INTO memories_fts(rowid, content, tags) VALUES (new.rowid, new.content, new.tags);
            END;

            CREATE INDEX IF NOT EXISTS idx_memories_vector_id ON memories(vector_id);
            CREATE INDEX IF NOT EXISTS idx_memories_channel    ON memories(channel);
            CREATE INDEX IF NOT EXISTS idx_memories_timestamp   ON memories(timestamp);"
        )?;

        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub fn save_text(
        &self,
        id: &str,
        vector_id: u64,
        content: &str,
        sender: &str,
        channel: &str,
        tags: &str,
        kind: &str,
        timestamp: i64,
    ) -> Result<()> {
        let vid = vector_id as i64;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO memories (id, vector_id, content, sender, channel, tags, kind, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, vid, content, sender, channel, tags, kind, timestamp],
        )?;
        Ok(())
    }

    pub fn get_text_by_vector_id(&self, vector_id: u64) -> Result<Option<String>> {
        let vid = vector_id as i64;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT content FROM memories WHERE vector_id = ?1")?;
        let mut rows = stmt.query(params![vid])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn get_text_by_vector_id_filtered(
        &self,
        vector_id: u64,
        filter_kind: Option<&str>,
    ) -> Result<Option<String>> {
        let vid = vector_id as i64;
        let conn = self.conn.lock().unwrap();
        match filter_kind {
            Some(kind) => {
                let mut stmt = conn.prepare(
                    "SELECT content FROM memories WHERE vector_id = ?1 AND kind = ?2"
                )?;
                let mut rows = stmt.query(params![vid, kind])?;
                if let Some(row) = rows.next()? {
                    Ok(Some(row.get(0)?))
                } else {
                    Ok(None)
                }
            }
            None => self.get_text_by_vector_id(vector_id),
        }
    }

    pub fn get_message_id_by_vector_id(&self, vector_id: u64) -> Result<Option<String>> {
        let vid = vector_id as i64;
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id FROM memories WHERE vector_id = ?1")?;
        let mut rows = stmt.query(params![vid])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
        Ok(n as usize)
    }

    pub fn count_by_channel(&self, channel: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE channel = ?1",
            params![channel],
            |row| row.get(0),
        )?;
        Ok(n as usize)
    }

    pub fn recent_by_channel(&self, channel: &str, limit: usize) -> Result<Vec<StoredMemory>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT content, tags FROM memories WHERE channel = ?1 ORDER BY timestamp DESC LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![channel, limit as i64], |row| {
            Ok(StoredMemory {
                content: row.get(0)?,
                tags: row.get(1)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_memory_by_id(&self, id: &str) -> Result<Option<StoredMemoryDetail>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, content, sender, channel, tags, kind, timestamp
             FROM memories WHERE id = ?1"
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(StoredMemoryDetail {
                id: row.get(0)?,
                content: row.get(1)?,
                sender: row.get(2)?,
                channel: row.get(3)?,
                tags: row.get(4)?,
                kind: row.get(5)?,
                timestamp: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }
}
