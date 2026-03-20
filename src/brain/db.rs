//! Brain-specific SQLite schema and CRUD operations.

use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;

/// Initialize or open the brain database with schema.
pub fn open_db(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open(db_path)?;

    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA mmap_size = 8388608;
         PRAGMA cache_size = -2000;
         PRAGMA temp_store = MEMORY;",
    )?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS brain_chunks (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            file_path    TEXT NOT NULL,
            file_hash    TEXT NOT NULL,
            chunk_index  INTEGER NOT NULL,
            chunk_key    TEXT NOT NULL,
            content      TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            token_count  INTEGER NOT NULL,
            category     TEXT NOT NULL,
            file_type    TEXT NOT NULL,
            subject      TEXT,
            active       INTEGER DEFAULT 1,
            embedding    BLOB,
            indexed_at   TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(file_path, chunk_index)
        );

        CREATE INDEX IF NOT EXISTS idx_brain_category_subject ON brain_chunks(category, subject);
        CREATE INDEX IF NOT EXISTS idx_brain_active ON brain_chunks(active);
        CREATE INDEX IF NOT EXISTS idx_brain_file_path ON brain_chunks(file_path);",
    )?;

    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS brain_chunks_fts USING fts5(
            chunk_key, content, content='brain_chunks', content_rowid='id'
        );",
    )?;

    // Triggers to keep FTS5 in sync
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS brain_fts_insert AFTER INSERT ON brain_chunks BEGIN
            INSERT INTO brain_chunks_fts(rowid, chunk_key, content)
                VALUES (new.id, new.chunk_key, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS brain_fts_delete AFTER DELETE ON brain_chunks BEGIN
            INSERT INTO brain_chunks_fts(brain_chunks_fts, rowid, chunk_key, content)
                VALUES('delete', old.id, old.chunk_key, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS brain_fts_update AFTER UPDATE ON brain_chunks BEGIN
            INSERT INTO brain_chunks_fts(brain_chunks_fts, rowid, chunk_key, content)
                VALUES('delete', old.id, old.chunk_key, old.content);
            INSERT INTO brain_chunks_fts(rowid, chunk_key, content)
                VALUES (new.id, new.chunk_key, new.content);
        END;",
    )?;

    Ok(conn)
}

/// A chunk stored in the database.
#[derive(Debug, Clone)]
pub struct StoredChunk {
    pub id: i64,
    pub file_path: String,
    pub file_hash: String,
    pub chunk_index: i32,
    pub chunk_key: String,
    pub content: String,
    pub content_hash: String,
    pub token_count: i32,
    pub category: String,
    pub file_type: String,
    pub subject: Option<String>,
    pub active: bool,
    pub embedding: Option<Vec<u8>>,
    pub score: Option<f64>,
}

/// Insert or replace a chunk.
#[allow(clippy::too_many_arguments)]
pub fn upsert_chunk(
    conn: &Connection,
    file_path: &str,
    file_hash: &str,
    chunk_index: i32,
    chunk_key: &str,
    content: &str,
    content_hash: &str,
    token_count: i32,
    category: &str,
    file_type: &str,
    subject: Option<&str>,
    embedding: Option<&[u8]>,
) -> Result<i64> {
    // Delete existing chunk at this position (triggers FTS cleanup)
    conn.execute(
        "DELETE FROM brain_chunks WHERE file_path = ?1 AND chunk_index = ?2",
        params![file_path, chunk_index],
    )?;

    conn.execute(
        "INSERT INTO brain_chunks
            (file_path, file_hash, chunk_index, chunk_key, content, content_hash,
             token_count, category, file_type, subject, embedding)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            file_path,
            file_hash,
            chunk_index,
            chunk_key,
            content,
            content_hash,
            token_count,
            category,
            file_type,
            subject,
            embedding,
        ],
    )?;

    Ok(conn.last_insert_rowid())
}

/// Remove all chunks for a file.
pub fn remove_file_chunks(conn: &Connection, file_path: &str) -> Result<usize> {
    let count = conn.execute(
        "DELETE FROM brain_chunks WHERE file_path = ?1",
        params![file_path],
    )?;
    Ok(count)
}

/// Get the stored file hash for a path (for incremental indexing).
pub fn get_file_hash(conn: &Connection, file_path: &str) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT file_hash FROM brain_chunks WHERE file_path = ?1 LIMIT 1")?;
    let hash = stmt.query_row(params![file_path], |row| row.get(0)).ok();
    Ok(hash)
}

/// Get chunk count, file count, and stale count statistics.
pub fn stats(conn: &Connection) -> Result<(usize, usize, usize)> {
    let chunk_count: usize = conn.query_row(
        "SELECT COUNT(*) FROM brain_chunks WHERE active = 1",
        [],
        |row| row.get(0),
    )?;
    let file_count: usize = conn.query_row(
        "SELECT COUNT(DISTINCT file_path) FROM brain_chunks WHERE active = 1",
        [],
        |row| row.get(0),
    )?;
    let stale_count: usize = conn.query_row(
        "SELECT COUNT(*) FROM brain_chunks WHERE active = 0",
        [],
        |row| row.get(0),
    )?;
    Ok((chunk_count, file_count, stale_count))
}

/// Get all unique file paths currently indexed.
pub fn indexed_files(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT file_path FROM brain_chunks")?;
    let files = stmt
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(files)
}

/// FTS5 keyword search. Returns (chunk_id, BM25 score).
pub fn fts_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<(i64, f32)>> {
    let fts_query = sanitize_fts_query(query);
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT bc.id, -rank as score
         FROM brain_chunks_fts fts
         JOIN brain_chunks bc ON bc.id = fts.rowid
         WHERE brain_chunks_fts MATCH ?1
           AND bc.active = 1
         ORDER BY rank
         LIMIT ?2",
    )?;

    let results = stmt
        .query_map(params![fts_query, limit as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f32>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(results)
}

/// FTS5 keyword search with optional subject and category filters.
pub fn fts_search_filtered(
    conn: &Connection,
    query: &str,
    subject: Option<&str>,
    categories: &[&str],
    limit: usize,
) -> Result<Vec<(i64, f32)>> {
    let fts_query = sanitize_fts_query(query);
    if fts_query.is_empty() {
        return Ok(Vec::new());
    }

    let mut sql = String::from(
        "SELECT bc.id, -rank as score
         FROM brain_chunks_fts fts
         JOIN brain_chunks bc ON bc.id = fts.rowid
         WHERE brain_chunks_fts MATCH ?1
           AND bc.active = 1",
    );

    if subject.is_some() {
        sql.push_str(" AND (bc.subject = ?3 OR bc.subject IS NULL)");
    }

    if !categories.is_empty() {
        // Categories are from a known enum, safe to inline
        let cat_list = categories
            .iter()
            .map(|c| format!("'{}'", c.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        use std::fmt::Write;
        write!(sql, " AND bc.category IN ({cat_list})").unwrap();
    }

    sql.push_str(" ORDER BY rank LIMIT ?2");

    let mut stmt = conn.prepare(&sql)?;
    let limit_i64 = limit as i64;

    let results = if let Some(subj) = subject {
        stmt.query_map(params![fts_query, limit_i64, subj], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f32>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect()
    } else {
        stmt.query_map(params![fts_query, limit_i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f32>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect()
    };

    Ok(results)
}

/// Get a chunk by ID.
pub fn get_chunk(conn: &Connection, id: i64) -> Result<Option<StoredChunk>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_path, file_hash, chunk_index, chunk_key, content, content_hash,
                token_count, category, file_type, subject, active, embedding
         FROM brain_chunks WHERE id = ?1",
    )?;

    let chunk = stmt.query_row(params![id], map_chunk).ok();

    Ok(chunk)
}

/// Get all active chunks with embeddings, optionally filtered.
pub fn get_chunks_with_embeddings(
    conn: &Connection,
    subject: Option<&str>,
    categories: &[&str],
) -> Result<Vec<StoredChunk>> {
    let mut sql = String::from(
        "SELECT id, file_path, file_hash, chunk_index, chunk_key, content, content_hash,
                token_count, category, file_type, subject, active, embedding
         FROM brain_chunks
         WHERE active = 1 AND embedding IS NOT NULL",
    );

    if subject.is_some() {
        sql.push_str(" AND (subject = ?1 OR subject IS NULL)");
    }

    if !categories.is_empty() {
        let cat_list = categories
            .iter()
            .map(|c| format!("'{}'", c.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        use std::fmt::Write;
        write!(sql, " AND category IN ({cat_list})").unwrap();
    }

    let mut stmt = conn.prepare(&sql)?;

    let chunks = if let Some(subj) = subject {
        stmt.query_map(params![subj], map_chunk)?
            .filter_map(|r| r.ok())
            .collect()
    } else {
        stmt.query_map([], map_chunk)?
            .filter_map(|r| r.ok())
            .collect()
    };

    Ok(chunks)
}

fn map_chunk(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredChunk> {
    Ok(StoredChunk {
        id: row.get(0)?,
        file_path: row.get(1)?,
        file_hash: row.get(2)?,
        chunk_index: row.get(3)?,
        chunk_key: row.get(4)?,
        content: row.get(5)?,
        content_hash: row.get(6)?,
        token_count: row.get(7)?,
        category: row.get(8)?,
        file_type: row.get(9)?,
        subject: row.get(10)?,
        active: row.get::<_, i32>(11)? == 1,
        embedding: row.get(12)?,
        score: None,
    })
}

/// Sanitize a query string for FTS5.
fn sanitize_fts_query(query: &str) -> String {
    // Replace hyphens with spaces — FTS5 treats `-` as NOT operator
    let dehyphenated = query.replace('-', " ");
    dehyphenated
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn open_db_creates_schema() {
        let dir = TempDir::new().unwrap();
        let conn = open_db(&dir.path().join("brain.db")).unwrap();
        let count: i32 = conn
            .query_row("SELECT COUNT(*) FROM brain_chunks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn upsert_and_get_chunk() {
        let dir = TempDir::new().unwrap();
        let conn = open_db(&dir.path().join("brain.db")).unwrap();

        let id = upsert_chunk(
            &conn,
            "soul/identity.yaml",
            "abc123",
            0,
            "identity",
            "name: Joel",
            "def456",
            3,
            "soul",
            "yaml",
            None,
            None,
        )
        .unwrap();

        let chunk = get_chunk(&conn, id).unwrap().unwrap();
        assert_eq!(chunk.file_path, "soul/identity.yaml");
        assert_eq!(chunk.chunk_key, "identity");
        assert_eq!(chunk.category, "soul");
        assert!(chunk.subject.is_none());
    }

    #[test]
    fn upsert_replaces_existing() {
        let dir = TempDir::new().unwrap();
        let conn = open_db(&dir.path().join("brain.db")).unwrap();

        upsert_chunk(
            &conn,
            "a.yaml",
            "h1",
            0,
            "k",
            "old content",
            "ch1",
            2,
            "soul",
            "yaml",
            None,
            None,
        )
        .unwrap();
        let id2 = upsert_chunk(
            &conn,
            "a.yaml",
            "h2",
            0,
            "k",
            "new content",
            "ch2",
            2,
            "soul",
            "yaml",
            None,
            None,
        )
        .unwrap();

        let chunk = get_chunk(&conn, id2).unwrap().unwrap();
        assert_eq!(chunk.content, "new content");
        assert_eq!(chunk.file_hash, "h2");
    }

    #[test]
    fn stats_counts_correctly() {
        let dir = TempDir::new().unwrap();
        let conn = open_db(&dir.path().join("brain.db")).unwrap();

        upsert_chunk(
            &conn, "a.yaml", "h1", 0, "k", "c", "ch", 1, "soul", "yaml", None, None,
        )
        .unwrap();
        upsert_chunk(
            &conn, "a.yaml", "h1", 1, "k2", "c2", "ch2", 1, "soul", "yaml", None, None,
        )
        .unwrap();
        upsert_chunk(
            &conn,
            "b.yaml",
            "h2",
            0,
            "k",
            "c",
            "ch",
            1,
            "cortex",
            "yaml",
            Some("backend"),
            None,
        )
        .unwrap();

        let (chunks, files, stale) = stats(&conn).unwrap();
        assert_eq!(chunks, 3);
        assert_eq!(files, 2);
        assert_eq!(stale, 0);
    }

    #[test]
    fn fts_search_finds_content() {
        let dir = TempDir::new().unwrap();
        let conn = open_db(&dir.path().join("brain.db")).unwrap();

        upsert_chunk(
            &conn,
            "a.yaml",
            "h",
            0,
            "identity",
            "Joel is a cinematographer",
            "ch",
            5,
            "soul",
            "yaml",
            None,
            None,
        )
        .unwrap();
        upsert_chunk(
            &conn,
            "b.yaml",
            "h",
            0,
            "backend",
            "Django multi-tenant database",
            "ch2",
            4,
            "knowledge",
            "yaml",
            None,
            None,
        )
        .unwrap();

        let results = fts_search(&conn, "cinematographer", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn remove_file_chunks_works() {
        let dir = TempDir::new().unwrap();
        let conn = open_db(&dir.path().join("brain.db")).unwrap();

        upsert_chunk(
            &conn, "a.yaml", "h", 0, "k", "c", "ch", 1, "soul", "yaml", None, None,
        )
        .unwrap();
        upsert_chunk(
            &conn, "a.yaml", "h", 1, "k2", "c2", "ch2", 1, "soul", "yaml", None, None,
        )
        .unwrap();

        let removed = remove_file_chunks(&conn, "a.yaml").unwrap();
        assert_eq!(removed, 2);

        let (chunks, _, _) = stats(&conn).unwrap();
        assert_eq!(chunks, 0);
    }

    #[test]
    fn sanitize_fts_query_cleans_input() {
        assert_eq!(sanitize_fts_query("hello world"), "hello world");
        assert_eq!(sanitize_fts_query("multi-tenant"), "multi tenant");
        assert_eq!(
            sanitize_fts_query("test (AND) \"quotes\""),
            "test AND quotes"
        );
        assert_eq!(sanitize_fts_query(""), "");
    }
}
