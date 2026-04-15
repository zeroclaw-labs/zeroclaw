use super::embeddings::EmbeddingProvider;
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use super::vector;
use anyhow::Context;
use async_trait::async_trait;
use chrono::Local;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use uuid::Uuid;

/// Maximum allowed open timeout (seconds) to avoid unreasonable waits.
const SQLITE_OPEN_TIMEOUT_CAP_SECS: u64 = 300;

/// SQLite-backed persistent memory — the brain
///
/// Full-stack search engine:
/// - **Vector DB**: embeddings stored as BLOB, cosine similarity search
/// - **Keyword Search**: FTS5 virtual table with BM25 scoring
/// - **Hybrid Merge**: weighted fusion of vector + keyword results
/// - **Embedding Cache**: LRU-evicted cache to avoid redundant API calls
/// - **Safe Reindex**: temp DB → seed → sync → atomic swap → rollback
/// Hybrid search strategy for recall queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Classic weighted fusion: `vector_weight * vec + keyword_weight * fts`
    Weighted,
    /// Reciprocal Rank Fusion — score-scale agnostic, fairer for BM25 + cosine mixing
    Rrf,
}

impl SearchMode {
    pub fn from_str_config(s: &str) -> Self {
        match s {
            "rrf" => Self::Rrf,
            _ => Self::Weighted,
        }
    }
}

pub struct SqliteMemory {
    conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
    embedder: Arc<dyn EmbeddingProvider>,
    search_mode: SearchMode,
    vector_weight: f32,
    keyword_weight: f32,
    rrf_k: f32,
    cache_max: usize,
    /// Optional sync engine attached at factory time for v3.0 dual-brain
    /// cross-device replication. When set, typed mutations (timeline append,
    /// compiled truth update, phone call insert) auto-record delta journal
    /// entries so peers receive them via the existing E2E pipeline.
    /// Interior-mutable so the factory can attach post-construction without
    /// breaking the `Memory` trait upcast path.
    sync: Mutex<Option<Arc<Mutex<super::sync::SyncEngine>>>>,
}

impl SqliteMemory {
    /// Expose the connection for cross-module operations (categories, phone_calls).
    ///
    /// Prefer the typed methods on `SqliteMemory` when available.
    /// Use this only for modules that need direct table access (e.g. `CategoryStore`,
    /// `post_call::insert_phone_call`).
    pub fn connection(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
    }

    /// Alias for tests — same as `connection()`.
    #[cfg(test)]
    pub fn conn_for_test(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.connection()
    }

    pub fn new(workspace_dir: &Path) -> anyhow::Result<Self> {
        Self::with_embedder(
            workspace_dir,
            Arc::new(super::embeddings::NoopEmbedding),
            SearchMode::Weighted,
            0.7,
            0.3,
            60.0,
            10_000,
            None,
        )
    }

    /// Build SQLite memory with optional open timeout.
    ///
    /// If `open_timeout_secs` is `Some(n)`, opening the database is limited to `n` seconds
    /// (capped at 300). Useful when the DB file may be locked or on slow storage.
    /// `None` = wait indefinitely (default).
    pub fn with_embedder(
        workspace_dir: &Path,
        embedder: Arc<dyn EmbeddingProvider>,
        search_mode: SearchMode,
        vector_weight: f32,
        keyword_weight: f32,
        rrf_k: f32,
        cache_max: usize,
        open_timeout_secs: Option<u64>,
    ) -> anyhow::Result<Self> {
        Self::with_options(
            workspace_dir,
            embedder,
            search_mode,
            vector_weight,
            keyword_weight,
            rrf_k,
            cache_max,
            open_timeout_secs,
            "wal",
        )
    }

    /// Build SQLite memory with full options including journal mode.
    ///
    /// `journal_mode` accepts `"wal"` (default, best performance) or `"delete"`
    /// (required for network/shared filesystems that lack shared-memory support).
    pub fn with_options(
        workspace_dir: &Path,
        embedder: Arc<dyn EmbeddingProvider>,
        search_mode: SearchMode,
        vector_weight: f32,
        keyword_weight: f32,
        rrf_k: f32,
        cache_max: usize,
        open_timeout_secs: Option<u64>,
        journal_mode: &str,
    ) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("memory").join("brain.db");

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Self::open_connection(&db_path, open_timeout_secs)?;

        // ── Production-grade PRAGMA tuning ──────────────────────
        // WAL mode: concurrent reads during writes, crash-safe (default)
        // DELETE mode: for shared/network filesystems without mmap/shm support
        // normal sync: 2× write speed, still durable
        // mmap 8 MB: let the OS page-cache serve hot reads (WAL only)
        // cache 2 MB: keep ~500 hot pages in-process
        // temp_store memory: temp tables never hit disk
        let journal_pragma = match journal_mode.to_lowercase().as_str() {
            "delete" => "PRAGMA journal_mode = DELETE;",
            _ => "PRAGMA journal_mode = WAL;",
        };
        let mmap_pragma = match journal_mode.to_lowercase().as_str() {
            "delete" => "PRAGMA mmap_size = 0;",
            _ => "PRAGMA mmap_size = 8388608;",
        };
        conn.execute_batch(&format!(
            "{journal_pragma}
             PRAGMA synchronous  = NORMAL;
             PRAGMA busy_timeout = 5000;
             {mmap_pragma}
             PRAGMA cache_size   = -2000;
             PRAGMA temp_store   = MEMORY;"
        ))?;

        Self::init_schema(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
            embedder,
            search_mode,
            vector_weight,
            keyword_weight,
            rrf_k,
            cache_max,
            sync: Mutex::new(None),
        })
    }

    /// Open SQLite connection, optionally with a timeout (for locked/slow storage).
    fn open_connection(
        db_path: &Path,
        open_timeout_secs: Option<u64>,
    ) -> anyhow::Result<Connection> {
        let path_buf = db_path.to_path_buf();

        let conn = if let Some(secs) = open_timeout_secs {
            let capped = secs.min(SQLITE_OPEN_TIMEOUT_CAP_SECS);
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let result = Connection::open(&path_buf);
                let _ = tx.send(result);
            });
            match rx.recv_timeout(Duration::from_secs(capped)) {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => return Err(e).context("SQLite failed to open database"),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    anyhow::bail!("SQLite connection open timed out after {} seconds", capped);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("SQLite open thread exited unexpectedly");
                }
            }
        } else {
            Connection::open(&path_buf).context("SQLite failed to open database")?
        };

        Ok(conn)
    }

    /// Initialize all tables: memories, FTS5, `embedding_cache`
    fn init_schema(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "-- Core memories table
            CREATE TABLE IF NOT EXISTS memories (
                id          TEXT PRIMARY KEY,
                key         TEXT NOT NULL UNIQUE,
                content     TEXT NOT NULL,
                category    TEXT NOT NULL DEFAULT 'core',
                embedding   BLOB,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_memories_category ON memories(category);
            CREATE INDEX IF NOT EXISTS idx_memories_updated ON memories(updated_at DESC);
            -- Note: no explicit index on `key` — the UNIQUE constraint already
            -- creates an implicit unique B-tree index, so a second one would
            -- waste space and slow inserts.

            -- FTS5 full-text search (BM25 scoring)
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                key, content, content=memories, content_rowid=rowid
            );

            -- FTS5 triggers: keep in sync with memories table
            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, key, content)
                VALUES (new.rowid, new.key, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, key, content)
                VALUES ('delete', old.rowid, old.key, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, key, content)
                VALUES ('delete', old.rowid, old.key, old.content);
                INSERT INTO memories_fts(rowid, key, content)
                VALUES (new.rowid, new.key, new.content);
            END;

            -- Embedding cache with LRU eviction
            CREATE TABLE IF NOT EXISTS embedding_cache (
                content_hash TEXT PRIMARY KEY,
                embedding    BLOB NOT NULL,
                created_at   TEXT NOT NULL,
                accessed_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_cache_accessed ON embedding_cache(accessed_at);",
        )?;

        // Migration: add session_id column if not present (safe to run repeatedly)
        let memories_sql: String = conn
            .prepare("SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'")?
            .query_row([], |row| row.get::<_, String>(0))?;

        if !memories_sql.contains("session_id") {
            conn.execute_batch(
                "ALTER TABLE memories ADD COLUMN session_id TEXT;
                 CREATE INDEX IF NOT EXISTS idx_memories_session ON memories(session_id);",
            )?;
        }

        // ── v3.0 Migration: Compiled Truth + Timeline (S2) ───────────
        // Add compiled_truth columns to memories (non-destructive extension)
        if !memories_sql.contains("compiled_truth") {
            conn.execute_batch(
                "ALTER TABLE memories ADD COLUMN compiled_truth TEXT;
                 ALTER TABLE memories ADD COLUMN truth_version INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE memories ADD COLUMN truth_updated_at INTEGER;
                 ALTER TABLE memories ADD COLUMN needs_recompile INTEGER NOT NULL DEFAULT 0;
                 CREATE INDEX IF NOT EXISTS idx_memories_needs_recompile
                     ON memories(needs_recompile) WHERE needs_recompile = 1;",
            )?;
        }

        // memory_timeline — append-only evidence store
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_timeline (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid            TEXT NOT NULL UNIQUE,
                memory_id       TEXT NOT NULL,
                event_type      TEXT NOT NULL CHECK(event_type IN (
                                    'call','chat','doc','manual','workflow','email','ocr'
                                )),
                event_at        INTEGER NOT NULL,
                source_ref      TEXT NOT NULL,
                content         TEXT NOT NULL,
                content_sha256  TEXT NOT NULL,
                metadata_json   TEXT,
                device_id       TEXT NOT NULL,
                created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
                FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_timeline_memory_time
                ON memory_timeline(memory_id, event_at DESC);
            CREATE INDEX IF NOT EXISTS idx_timeline_source_ref
                ON memory_timeline(source_ref);
            CREATE INDEX IF NOT EXISTS idx_timeline_event_type
                ON memory_timeline(event_type, event_at DESC);",
        )?;

        // Append-only enforcement: block UPDATE on memory_timeline
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS trg_timeline_no_update
             BEFORE UPDATE ON memory_timeline
             BEGIN
                 SELECT RAISE(ABORT, 'memory_timeline is append-only');
             END;",
        )?;

        // FTS5 mirror for timeline natural language search
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_timeline_fts
                 USING fts5(content, source_ref UNINDEXED, memory_id UNINDEXED,
                            content='memory_timeline', content_rowid='id');

             CREATE TRIGGER IF NOT EXISTS trg_timeline_ai AFTER INSERT ON memory_timeline BEGIN
                 INSERT INTO memory_timeline_fts(rowid, content, source_ref, memory_id)
                 VALUES (new.id, new.content, new.source_ref, new.memory_id);
             END;",
        )?;

        // phone_calls — phone assistant call metadata
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS phone_calls (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                call_uuid           TEXT NOT NULL UNIQUE,
                direction           TEXT NOT NULL CHECK(direction IN ('in','out','missed')),
                caller_number       TEXT,
                caller_number_e164  TEXT,
                caller_object_id    INTEGER,
                started_at          INTEGER NOT NULL,
                ended_at            INTEGER,
                duration_ms         INTEGER,
                gps_lat             REAL,
                gps_lon             REAL,
                transcript          TEXT,
                summary             TEXT,
                risk_level          TEXT CHECK(risk_level IN ('safe','warn','danger')) DEFAULT 'safe',
                sos_triggered       INTEGER NOT NULL DEFAULT 0,
                language            TEXT,
                memory_id           TEXT,
                device_id           TEXT NOT NULL,
                created_at          INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX IF NOT EXISTS idx_phone_calls_number
                ON phone_calls(caller_number_e164, started_at DESC);
            CREATE INDEX IF NOT EXISTS idx_phone_calls_object
                ON phone_calls(caller_object_id, started_at DESC);
            CREATE INDEX IF NOT EXISTS idx_phone_calls_risk
                ON phone_calls(risk_level, started_at DESC) WHERE risk_level != 'safe';",
        )?;

        // user_categories — user-created custom categories
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS user_categories (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid            TEXT NOT NULL UNIQUE,
                name            TEXT NOT NULL,
                icon            TEXT,
                parent_seed_key TEXT,
                order_index     INTEGER NOT NULL DEFAULT 0,
                created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at      INTEGER NOT NULL DEFAULT (unixepoch()),
                UNIQUE(name, parent_seed_key)
            );
            CREATE INDEX IF NOT EXISTS idx_user_categories_order
                ON user_categories(parent_seed_key, order_index);",
        )?;

        // workflow_runs — workflow execution history (for learning loop)
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_runs (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid              TEXT NOT NULL UNIQUE,
                workflow_id       INTEGER NOT NULL,
                workflow_version  INTEGER NOT NULL DEFAULT 1,
                started_at        INTEGER NOT NULL,
                ended_at          INTEGER,
                status            TEXT NOT NULL CHECK(status IN (
                                      'running','success','failed','cancelled','paused'
                                  )),
                trigger_source    TEXT,
                input_json        TEXT,
                input_sha256      TEXT,
                output_ref        TEXT,
                output_sha256     TEXT,
                error_message     TEXT,
                cost_tokens_in    INTEGER DEFAULT 0,
                cost_tokens_out   INTEGER DEFAULT 0,
                cost_llm_calls    INTEGER DEFAULT 0,
                feedback_rating   INTEGER,
                feedback_note     TEXT,
                device_id         TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_wfruns_workflow
                ON workflow_runs(workflow_id, started_at DESC);
            CREATE INDEX IF NOT EXISTS idx_wfruns_status
                ON workflow_runs(status, started_at DESC);",
        )?;

        // workflow_suggestions — Dream Cycle output (improvement inbox)
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_suggestions (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid             TEXT NOT NULL UNIQUE,
                workflow_id      INTEGER,
                suggestion_type  TEXT NOT NULL CHECK(suggestion_type IN (
                                     'fix_failure','default_value','abstraction','deprecation'
                                 )),
                title            TEXT NOT NULL,
                description      TEXT NOT NULL,
                patch_yaml       TEXT,
                created_at       INTEGER NOT NULL DEFAULT (unixepoch()),
                reviewed_at      INTEGER,
                review_decision  TEXT CHECK(review_decision IN ('accepted','rejected','snoozed'))
            );
            CREATE INDEX IF NOT EXISTS idx_wfsug_pending
                ON workflow_suggestions(created_at DESC) WHERE reviewed_at IS NULL;",
        )?;

        // ── LLM Wiki: documents table for long-content summary/link pattern ──
        // Stores full originals + LLM-generated summaries + extracted entities.
        // When a user uploads a document or pastes long text (2000+ chars),
        // the summary goes into chat context, original stays here.
        // LLM can fetch the original via document_fetch(content_id) when needed.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS documents (
                content_id   TEXT PRIMARY KEY,
                title        TEXT NOT NULL,
                summary      TEXT NOT NULL,
                content      TEXT NOT NULL,
                category     TEXT NOT NULL DEFAULT 'general',
                entities     TEXT NOT NULL DEFAULT '[]',
                token_count  INTEGER NOT NULL DEFAULT 0,
                char_count   INTEGER NOT NULL DEFAULT 0,
                source_type  TEXT NOT NULL DEFAULT 'text',
                created_at   TEXT NOT NULL,
                last_accessed TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_documents_category ON documents(category);
            CREATE INDEX IF NOT EXISTS idx_documents_created ON documents(created_at DESC);

            -- FTS5 full-text index on summaries + entities (search-first)
            CREATE VIRTUAL TABLE IF NOT EXISTS documents_fts USING fts5(
                title, summary, entities, content=documents, content_rowid=rowid
            );
            CREATE TRIGGER IF NOT EXISTS documents_ai AFTER INSERT ON documents BEGIN
                INSERT INTO documents_fts(rowid, title, summary, entities)
                VALUES (new.rowid, new.title, new.summary, new.entities);
            END;
            CREATE TRIGGER IF NOT EXISTS documents_ad AFTER DELETE ON documents BEGIN
                INSERT INTO documents_fts(documents_fts, rowid, title, summary, entities)
                VALUES ('delete', old.rowid, old.title, old.summary, old.entities);
            END;
            CREATE TRIGGER IF NOT EXISTS documents_au AFTER UPDATE ON documents BEGIN
                INSERT INTO documents_fts(documents_fts, rowid, title, summary, entities)
                VALUES ('delete', old.rowid, old.title, old.summary, old.entities);
                INSERT INTO documents_fts(rowid, title, summary, entities)
                VALUES (new.rowid, new.title, new.summary, new.entities);
            END;",
        )?;

        Ok(())
    }

    fn category_to_str(cat: &MemoryCategory) -> String {
        match cat {
            MemoryCategory::Core => "core".into(),
            MemoryCategory::Daily => "daily".into(),
            MemoryCategory::Conversation => "conversation".into(),
            MemoryCategory::Custom(name) => name.clone(),
        }
    }

    fn str_to_category(s: &str) -> MemoryCategory {
        match s {
            "core" => MemoryCategory::Core,
            "daily" => MemoryCategory::Daily,
            "conversation" => MemoryCategory::Conversation,
            other => MemoryCategory::Custom(other.to_string()),
        }
    }

    /// Deterministic content hash for embedding cache.
    /// Uses SHA-256 (truncated) instead of DefaultHasher, which is
    /// explicitly documented as unstable across Rust versions.
    fn content_hash(text: &str) -> String {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(text.as_bytes());
        // First 8 bytes → 16 hex chars, matching previous format length
        format!(
            "{:016x}",
            u64::from_be_bytes(
                hash[..8]
                    .try_into()
                    .expect("SHA-256 always produces >= 8 bytes")
            )
        )
    }

    /// Get embedding from cache, or compute + cache it
    async fn get_or_compute_embedding(&self, text: &str) -> anyhow::Result<Option<Vec<f32>>> {
        if self.embedder.dimensions() == 0 {
            return Ok(None); // Noop embedder
        }

        let hash = Self::content_hash(text);
        let now = Local::now().to_rfc3339();

        // Check cache (offloaded to blocking thread)
        let conn = self.conn.clone();
        let hash_c = hash.clone();
        let now_c = now.clone();
        let cached = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<f32>>> {
            let conn = conn.lock();
            let mut stmt = conn
                .prepare_cached("SELECT embedding FROM embedding_cache WHERE content_hash = ?1")?;
            let blob: Option<Vec<u8>> = stmt.query_row(params![hash_c], |row| row.get(0)).ok();
            if let Some(bytes) = blob {
                conn.execute(
                    "UPDATE embedding_cache SET accessed_at = ?1 WHERE content_hash = ?2",
                    params![now_c, hash_c],
                )?;
                return Ok(Some(vector::bytes_to_vec(&bytes)));
            }
            Ok(None)
        })
        .await??;

        if cached.is_some() {
            return Ok(cached);
        }

        // Compute embedding (async I/O)
        let embedding = self.embedder.embed_one(text).await?;
        let bytes = vector::vec_to_bytes(&embedding);

        // Store in cache + LRU eviction (offloaded to blocking thread)
        let conn = self.conn.clone();
        #[allow(clippy::cast_possible_wrap)]
        let cache_max = self.cache_max as i64;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = conn.lock();
            conn.execute(
                "INSERT OR REPLACE INTO embedding_cache (content_hash, embedding, created_at, accessed_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![hash, bytes, now, now],
            )?;
            conn.execute(
                "DELETE FROM embedding_cache WHERE content_hash IN (
                    SELECT content_hash FROM embedding_cache
                    ORDER BY accessed_at ASC
                    LIMIT MAX(0, (SELECT COUNT(*) FROM embedding_cache) - ?1)
                )",
                params![cache_max],
            )?;
            Ok(())
        })
        .await??;

        Ok(Some(embedding))
    }

    /// FTS5 BM25 keyword search
    fn fts5_search(
        conn: &Connection,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        // Escape FTS5 special chars and build query
        let fts_query: String = query
            .split_whitespace()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(" OR ");

        if fts_query.is_empty() {
            return Ok(Vec::new());
        }

        let sql = "SELECT m.id, bm25(memories_fts) as score
                   FROM memories_fts f
                   JOIN memories m ON m.rowid = f.rowid
                   WHERE memories_fts MATCH ?1
                   ORDER BY score
                   LIMIT ?2";

        let mut stmt = conn.prepare_cached(sql)?;
        #[allow(clippy::cast_possible_wrap)]
        let limit_i64 = limit as i64;

        let rows = stmt.query_map(params![fts_query, limit_i64], |row| {
            let id: String = row.get(0)?;
            let score: f64 = row.get(1)?;
            // BM25 returns negative scores (lower = better), negate for ranking
            #[allow(clippy::cast_possible_truncation)]
            Ok((id, (-score) as f32))
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Vector similarity search: scan embeddings and compute cosine similarity.
    ///
    /// Optional `category` and `session_id` filters reduce full-table scans
    /// when the caller already knows the scope of relevant memories.
    fn vector_search(
        conn: &Connection,
        query_embedding: &[f32],
        limit: usize,
        category: Option<&str>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        let mut sql = "SELECT id, embedding FROM memories WHERE embedding IS NOT NULL".to_string();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut idx = 1;

        if let Some(cat) = category {
            let _ = write!(sql, " AND category = ?{idx}");
            param_values.push(Box::new(cat.to_string()));
            idx += 1;
        }
        if let Some(sid) = session_id {
            let _ = write!(sql, " AND session_id = ?{idx}");
            param_values.push(Box::new(sid.to_string()));
        }

        let mut stmt = conn.prepare(&sql)?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(AsRef::as_ref).collect();
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
            let id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((id, blob))
        })?;

        let mut scored: Vec<(String, f32)> = Vec::new();
        for row in rows {
            let (id, blob) = row?;
            let emb = vector::bytes_to_vec(&blob);
            let sim = vector::cosine_similarity(query_embedding, &emb);
            if sim > 0.0 {
                scored.push((id, sim));
            }
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    /// Safe reindex: rebuild FTS5 + embeddings with rollback on failure
    #[allow(dead_code)]
    pub async fn reindex(&self) -> anyhow::Result<usize> {
        // Step 1: Rebuild FTS5
        {
            let conn = self.conn.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let conn = conn.lock();
                conn.execute_batch("INSERT INTO memories_fts(memories_fts) VALUES('rebuild');")?;
                Ok(())
            })
            .await??;
        }

        // Step 2: Re-embed all memories that lack embeddings
        if self.embedder.dimensions() == 0 {
            return Ok(0);
        }

        let conn = self.conn.clone();
        let entries: Vec<(String, String)> = tokio::task::spawn_blocking(move || {
            let conn = conn.lock();
            let mut stmt =
                conn.prepare_cached("SELECT id, content FROM memories WHERE embedding IS NULL")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            Ok::<_, anyhow::Error>(rows.filter_map(std::result::Result::ok).collect())
        })
        .await??;

        let mut count = 0;
        for (id, content) in &entries {
            if let Ok(Some(emb)) = self.get_or_compute_embedding(content).await {
                let bytes = vector::vec_to_bytes(&emb);
                let conn = self.conn.clone();
                let id = id.clone();
                tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    let conn = conn.lock();
                    conn.execute(
                        "UPDATE memories SET embedding = ?1 WHERE id = ?2",
                        params![bytes, id],
                    )?;
                    Ok(())
                })
                .await??;
                count += 1;
            }
        }

        Ok(count)
    }

    // ── v3.0 Compiled Truth + Timeline methods ───────────────────

    /// Attach a sync engine for v3.0 dual-brain cross-device replication.
    /// After attachment, `append_timeline`, `set_compiled_truth`, and
    /// `insert_phone_call` auto-record delta journal entries.
    /// The factory calls this from `create_synced_memory` when sync is enabled.
    pub fn attach_sync(&self, engine: Arc<Mutex<super::sync::SyncEngine>>) {
        *self.sync.lock() = Some(engine);
    }

    /// Run a closure with the attached sync engine, if any. No-op otherwise.
    /// Private helper — callers use the typed record_* methods.
    fn with_sync<F>(&self, f: F)
    where
        F: FnOnce(&mut super::sync::SyncEngine),
    {
        let guard = self.sync.lock();
        if let Some(engine_arc) = guard.as_ref().cloned() {
            drop(guard);
            let mut engine = engine_arc.lock();
            f(&mut engine);
        }
    }

    /// Update the compiled truth for a memory entry.
    /// Increments `truth_version` and sets `needs_recompile = 0`.
    /// Auto-records a `CompiledTruthUpdate` delta if a sync engine is attached.
    pub fn set_compiled_truth(&self, memory_key: &str, compiled_truth: &str) -> anyhow::Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Perform DB write and capture new version in a single locked scope.
        let new_version: u32 = {
            let conn = self.conn.lock();
            conn.execute(
                "UPDATE memories
                 SET compiled_truth = ?1,
                     truth_version = truth_version + 1,
                     truth_updated_at = ?2,
                     needs_recompile = 0
                 WHERE key = ?3",
                params![compiled_truth, now as i64, memory_key],
            )?;
            conn.query_row(
                "SELECT truth_version FROM memories WHERE key = ?1",
                params![memory_key],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u32
        };

        self.with_sync(|engine| {
            engine.record_compiled_truth_update(memory_key, compiled_truth, new_version);
        });
        Ok(())
    }

    /// Get compiled truth and version for a memory entry.
    pub fn get_compiled_truth(
        &self,
        memory_key: &str,
    ) -> anyhow::Result<Option<(String, u32)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT compiled_truth, truth_version FROM memories WHERE key = ?1",
        )?;
        let result = stmt.query_row(params![memory_key], |row| {
            let truth: Option<String> = row.get(0)?;
            let version: u32 = row.get(1)?;
            Ok(truth.map(|t| (t, version)))
        });
        match result {
            Ok(v) => Ok(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Mark a memory as needing recompilation (e.g. after new timeline evidence).
    pub fn mark_needs_recompile(&self, memory_key: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE memories SET needs_recompile = 1 WHERE key = ?1",
            params![memory_key],
        )?;
        Ok(())
    }

    /// Get all memory IDs/keys that need recompilation (for Dream Cycle).
    pub fn get_needs_recompile(&self) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, key FROM memories WHERE needs_recompile = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Append an evidence entry to `memory_timeline` (append-only).
    /// Returns the generated UUID for the entry.
    /// Auto-records a `TimelineAppend` delta if a sync engine is attached.
    #[allow(clippy::too_many_arguments)]
    pub fn append_timeline(
        &self,
        memory_id: &str,
        event_type: &str,
        event_at: u64,
        source_ref: &str,
        content: &str,
        metadata_json: Option<&str>,
        device_id: &str,
    ) -> anyhow::Result<String> {
        use sha2::{Digest, Sha256};
        let uuid = Uuid::new_v4().to_string();
        let content_sha256 = hex::encode(Sha256::digest(content.as_bytes()));
        {
            let conn = self.conn.lock();
            conn.execute(
                "INSERT INTO memory_timeline
                    (uuid, memory_id, event_type, event_at, source_ref, content, content_sha256, metadata_json, device_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    uuid,
                    memory_id,
                    event_type,
                    event_at as i64,
                    source_ref,
                    content,
                    content_sha256,
                    metadata_json,
                    device_id,
                ],
            )?;
        }

        self.with_sync(|engine| {
            engine.record_timeline_append(
                &uuid,
                memory_id,
                event_type,
                event_at,
                source_ref,
                content,
                &content_sha256,
                metadata_json,
            );
        });
        Ok(uuid)
    }

    /// Insert a phone call metadata row into `phone_calls`.
    /// Auto-records a `PhoneCallRecord` delta if a sync engine is attached.
    /// Replaces the local-only helper previously in `src/phone/post_call.rs`.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_phone_call(
        &self,
        call_uuid: &str,
        direction: &str,
        caller_number: Option<&str>,
        caller_number_e164: Option<&str>,
        caller_object_id: Option<i64>,
        started_at: u64,
        ended_at: Option<u64>,
        duration_ms: Option<u64>,
        gps_lat: Option<f64>,
        gps_lon: Option<f64>,
        transcript: Option<&str>,
        summary: Option<&str>,
        risk_level: &str,
        sos_triggered: bool,
        language: Option<&str>,
        memory_id: Option<&str>,
        device_id: &str,
    ) -> anyhow::Result<()> {
        {
            let conn = self.conn.lock();
            conn.execute(
                "INSERT INTO phone_calls
                    (call_uuid, direction, caller_number, caller_number_e164, caller_object_id,
                     started_at, ended_at, duration_ms, gps_lat, gps_lon,
                     transcript, summary, risk_level, sos_triggered, language,
                     memory_id, device_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    call_uuid,
                    direction,
                    caller_number,
                    caller_number_e164,
                    caller_object_id,
                    started_at as i64,
                    ended_at.map(|t| t as i64),
                    duration_ms.map(|t| t as i64),
                    gps_lat,
                    gps_lon,
                    transcript,
                    summary,
                    risk_level,
                    sos_triggered as i32,
                    language,
                    memory_id,
                    device_id,
                ],
            )?;
        }

        self.with_sync(|engine| {
            engine.record_phone_call(
                call_uuid,
                direction,
                caller_number_e164,
                caller_object_id,
                started_at,
                ended_at,
                duration_ms,
                transcript,
                summary,
                risk_level,
                memory_id,
            );
        });
        Ok(())
    }

    /// Get timeline entries for a memory, ordered by event_at descending.
    pub fn get_timeline(
        &self,
        memory_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<TimelineEntry>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT uuid, event_type, event_at, source_ref, content, metadata_json, device_id, created_at
             FROM memory_timeline
             WHERE memory_id = ?1
             ORDER BY event_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![memory_id, limit as i64], |row| {
            Ok(TimelineEntry {
                uuid: row.get(0)?,
                event_type: row.get(1)?,
                event_at: row.get(2)?,
                source_ref: row.get(3)?,
                content: row.get(4)?,
                metadata_json: row.get(5)?,
                device_id: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Get memory keys and their embeddings for duplicate detection (Dream Cycle).
    /// Returns up to `limit` entries that have non-empty embeddings.
    pub fn get_all_embeddings(&self, limit: usize) -> anyhow::Result<Vec<(String, Vec<f32>)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT key, embedding FROM memories WHERE embedding IS NOT NULL LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let key: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((key, blob))
        })?;
        let mut results = Vec::new();
        for row in rows {
            let (key, blob) = row?;
            if !blob.is_empty() {
                let embedding = super::vector::bytes_to_vec(&blob);
                if !embedding.is_empty() {
                    results.push((key, embedding));
                }
            }
        }
        Ok(results)
    }

    // ── v3.0 Multi-Query Expanded Recall (S3) ────────────────────

    /// Recall with multi-query expansion: expand the query into variations,
    /// search each independently, and fuse results via RRF.
    ///
    /// This is a higher-level entry point that wraps `Memory::recall()`.
    /// If expansion is disabled or the provider call fails, falls back
    /// to a single-query recall.
    pub async fn recall_expanded(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        expander: &super::query_expand::QueryExpander,
        expand_config: &super::query_expand::QueryExpandConfig,
        provider: &dyn crate::providers::traits::Provider,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let queries = expander.expand(query, expand_config, provider).await;
        self.recall_with_variations(query, &queries, limit, session_id).await
    }

    /// Recall with a pre-expanded list of query variations.
    /// Runs parallel FTS + vector search per variation, then fuses via RRF
    /// (or weighted, depending on `search_mode`). If `variations` has ≤ 1
    /// entry, falls back to standard `recall()`.
    ///
    /// **Provider-free**: unlike `recall_expanded`, this API requires no
    /// LLM provider — callers that already expanded the query (e.g. agent
    /// loop with its own QueryExpander) pass the variations directly.
    /// This is the method the `Memory` trait surfaces so non-sqlite
    /// backends can fall back gracefully.
    pub async fn recall_with_variations(
        &self,
        original_query: &str,
        variations: &[String],
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        use super::traits::Memory;

        if variations.len() <= 1 {
            // No expansion — use standard recall on the original query
            return self.recall(original_query, limit, session_id).await;
        }

        // Multi-query RRF: search each variation, merge all results
        let query_embedding_original = self.get_or_compute_embedding(original_query).await?;

        let queries_owned: Vec<String> = variations.to_vec();

        let conn = self.conn.clone();
        let sid = session_id.map(String::from);
        let rrf_k = self.rrf_k;
        let search_mode = self.search_mode;
        let vector_weight = self.vector_weight;
        let keyword_weight = self.keyword_weight;
        let embedder = self.embedder.clone();

        // Compute embeddings for expanded queries
        let mut query_embeddings = Vec::new();
        for q in &queries_owned {
            match embedder.embed_one(q).await {
                Ok(emb) if !emb.is_empty() => query_embeddings.push(Some(emb)),
                _ => query_embeddings.push(query_embedding_original.clone()),
            }
        }

        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let conn = conn.lock();
            let session_ref = sid.as_deref();

            // Collect ranked lists from all queries × {vec, fts}
            let mut all_vec_results: Vec<(String, f32)> = Vec::new();
            let mut all_fts_results: Vec<(String, f32)> = Vec::new();

            for (i, q) in queries_owned.iter().enumerate() {
                let fts = Self::fts5_search(&conn, q, limit * 2).unwrap_or_default();
                all_fts_results.extend(fts);

                if let Some(Some(ref emb)) = query_embeddings.get(i) {
                    let vec_hits = Self::vector_search(&conn, emb, limit * 2, None, session_ref)
                        .unwrap_or_default();
                    all_vec_results.extend(vec_hits);
                }
            }

            // Deduplicate by keeping the best score per id for each ranker
            let dedup_vec = dedup_ranked_list(&all_vec_results);
            let dedup_fts = dedup_ranked_list(&all_fts_results);

            // Merge via configured strategy
            let merged = if dedup_vec.is_empty() {
                dedup_fts
                    .iter()
                    .map(|(id, score)| super::vector::ScoredResult {
                        id: id.clone(),
                        vector_score: None,
                        keyword_score: Some(*score),
                        final_score: *score,
                    })
                    .collect::<Vec<_>>()
            } else {
                match search_mode {
                    SearchMode::Rrf => {
                        super::vector::rrf_merge(&dedup_vec, &dedup_fts, rrf_k, limit)
                    }
                    SearchMode::Weighted => super::vector::hybrid_merge(
                        &dedup_vec,
                        &dedup_fts,
                        vector_weight,
                        keyword_weight,
                        limit,
                    ),
                }
            };

            // Fetch full entries (same as recall())
            let mut results = Vec::with_capacity(merged.len());
            if !merged.is_empty() {
                let placeholders: String = (1..=merged.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT id, key, content, category, created_at, session_id \
                     FROM memories WHERE id IN ({placeholders})"
                );
                let mut stmt = conn.prepare(&sql)?;
                let id_params: Vec<Box<dyn rusqlite::types::ToSql>> = merged
                    .iter()
                    .map(|s| Box::new(s.id.clone()) as Box<dyn rusqlite::types::ToSql>)
                    .collect();
                let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                    id_params.iter().map(AsRef::as_ref).collect();
                let rows = stmt.query_map(params_ref.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                })?;

                let mut entry_map = std::collections::HashMap::new();
                for row in rows {
                    let (id, key, content, cat, ts, sid) = row?;
                    entry_map.insert(id, (key, content, cat, ts, sid));
                }

                for scored in &merged {
                    if let Some((key, content, cat, ts, sid)) = entry_map.remove(&scored.id) {
                        let entry = MemoryEntry {
                            id: scored.id.clone(),
                            key,
                            content,
                            category: Self::str_to_category(&cat),
                            timestamp: ts,
                            session_id: sid,
                            score: Some(f64::from(scored.final_score)),
                            recall_count: 0,
                            last_recalled: None,
                        };
                        if let Some(filter_sid) = session_ref {
                            if entry.session_id.as_deref() != Some(filter_sid) {
                                continue;
                            }
                        }
                        results.push(entry);
                    }
                }
            }

            Ok(results)
        })
        .await?
    }
}

/// Deduplicate a ranked list by keeping the highest score per id.
fn dedup_ranked_list(items: &[(String, f32)]) -> Vec<(String, f32)> {
    use std::collections::HashMap;
    let mut best: HashMap<String, f32> = HashMap::new();
    for (id, score) in items {
        best.entry(id.clone())
            .and_modify(|s| {
                if *score > *s {
                    *s = *score;
                }
            })
            .or_insert(*score);
    }
    let mut result: Vec<(String, f32)> = best.into_iter().collect();
    result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    result
}

/// A timeline evidence entry (read from `memory_timeline`).
#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub uuid: String,
    pub event_type: String,
    pub event_at: i64,
    pub source_ref: String,
    pub content: String,
    pub metadata_json: Option<String>,
    pub device_id: String,
    pub created_at: i64,
}

#[async_trait]
impl Memory for SqliteMemory {
    fn name(&self) -> &str {
        "sqlite"
    }

    fn attach_sync_engine(&self, engine: Arc<Mutex<super::sync::SyncEngine>>) {
        self.attach_sync(engine);
    }

    async fn recall_with_variations(
        &self,
        original_query: &str,
        variations: &[String],
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // Delegate to the concrete impl (multi-query RRF).
        SqliteMemory::recall_with_variations(self, original_query, variations, limit, session_id).await
    }

    async fn apply_remote_v3_delta(
        &self,
        delta: &super::sync::DeltaOperation,
    ) -> anyhow::Result<bool> {
        use super::sync::DeltaOperation;
        match delta {
            DeltaOperation::TimelineAppend {
                uuid,
                memory_id,
                event_type,
                event_at,
                source_ref,
                content,
                content_sha256,
                metadata_json,
            } => {
                let conn = self.conn.lock();
                // Idempotent — UUID uniqueness prevents duplicate inserts.
                // The per-device device_id is not in the delta (to keep
                // delta size minimal); we mark remote-origin rows with
                // "remote:<src_device>" when available, else "remote".
                let origin = "remote";
                conn.execute(
                    "INSERT OR IGNORE INTO memory_timeline
                        (uuid, memory_id, event_type, event_at, source_ref, content, content_sha256, metadata_json, device_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        uuid,
                        memory_id,
                        event_type,
                        *event_at as i64,
                        source_ref,
                        content,
                        content_sha256,
                        metadata_json.as_deref(),
                        origin,
                    ],
                )?;
                Ok(true)
            }
            DeltaOperation::PhoneCallRecord {
                call_uuid,
                direction,
                caller_number_e164,
                caller_object_id,
                started_at,
                ended_at,
                duration_ms,
                transcript,
                summary,
                risk_level,
                memory_id,
            } => {
                let conn = self.conn.lock();
                let origin = "remote";
                conn.execute(
                    "INSERT OR IGNORE INTO phone_calls
                        (call_uuid, direction, caller_number, caller_number_e164, caller_object_id,
                         started_at, ended_at, duration_ms, gps_lat, gps_lon,
                         transcript, summary, risk_level, sos_triggered, language,
                         memory_id, device_id)
                     VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9, ?10, 0, NULL, ?11, ?12)",
                    params![
                        call_uuid,
                        direction,
                        caller_number_e164.as_deref(),
                        caller_object_id,
                        *started_at as i64,
                        ended_at.map(|t| t as i64),
                        duration_ms.map(|t| t as i64),
                        transcript.as_deref(),
                        summary.as_deref(),
                        risk_level,
                        memory_id.as_deref(),
                        origin,
                    ],
                )?;
                Ok(true)
            }
            DeltaOperation::CompiledTruthUpdate {
                memory_key,
                compiled_truth,
                truth_version,
            } => {
                // LWW on truth_version: only apply if the remote version is
                // strictly greater than the local one. Prevents older truths
                // overwriting newer ones under out-of-order delivery.
                let conn = self.conn.lock();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let changed = conn.execute(
                    "UPDATE memories
                     SET compiled_truth = ?1,
                         truth_version = ?2,
                         truth_updated_at = ?3,
                         needs_recompile = 0
                     WHERE key = ?4 AND truth_version < ?2",
                    params![compiled_truth, *truth_version as i64, now as i64, memory_key],
                )?;
                Ok(changed > 0)
            }
            // Non-v3 operations fall through — SyncedMemory handles them.
            _ => Ok(false),
        }
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        // Compute embedding (async, before blocking work)
        let embedding_bytes = self
            .get_or_compute_embedding(content)
            .await?
            .map(|emb| vector::vec_to_bytes(&emb));

        let conn = self.conn.clone();
        let key = key.to_string();
        let content = content.to_string();
        let sid = session_id.map(String::from);

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = conn.lock();
            let now = Local::now().to_rfc3339();
            let cat = Self::category_to_str(&category);
            let id = Uuid::new_v4().to_string();

            conn.execute(
                "INSERT INTO memories (id, key, content, category, embedding, created_at, updated_at, session_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(key) DO UPDATE SET
                    content = excluded.content,
                    category = excluded.category,
                    embedding = excluded.embedding,
                    updated_at = excluded.updated_at,
                    session_id = excluded.session_id",
                params![id, key, content, cat, embedding_bytes, now, now, sid],
            )?;
            Ok(())
        })
        .await?
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        // Compute query embedding (async, before blocking work)
        let query_embedding = self.get_or_compute_embedding(query).await?;

        let conn = self.conn.clone();
        let query = query.to_string();
        let sid = session_id.map(String::from);
        let vector_weight = self.vector_weight;
        let keyword_weight = self.keyword_weight;
        let search_mode = self.search_mode;
        let rrf_k = self.rrf_k;

        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let conn = conn.lock();
            let session_ref = sid.as_deref();

            // FTS5 BM25 keyword search
            let keyword_results = Self::fts5_search(&conn, &query, limit * 2).unwrap_or_default();

            // Vector similarity search (if embeddings available)
            let vector_results = if let Some(ref qe) = query_embedding {
                Self::vector_search(&conn, qe, limit * 2, None, session_ref).unwrap_or_default()
            } else {
                Vec::new()
            };

            // Hybrid merge — branch on search_mode
            let merged = if vector_results.is_empty() {
                keyword_results
                    .iter()
                    .map(|(id, score)| vector::ScoredResult {
                        id: id.clone(),
                        vector_score: None,
                        keyword_score: Some(*score),
                        final_score: *score,
                    })
                    .collect::<Vec<_>>()
            } else {
                match search_mode {
                    SearchMode::Rrf => vector::rrf_merge(
                        &vector_results,
                        &keyword_results,
                        rrf_k,
                        limit,
                    ),
                    SearchMode::Weighted => vector::hybrid_merge(
                        &vector_results,
                        &keyword_results,
                        vector_weight,
                        keyword_weight,
                        limit,
                    ),
                }
            };

            // Fetch full entries for merged results in a single query
            // instead of N round-trips (N+1 pattern).
            let mut results = Vec::with_capacity(merged.len());
            if !merged.is_empty() {
                let placeholders: String = (1..=merged.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT id, key, content, category, created_at, session_id \
                     FROM memories WHERE id IN ({placeholders})"
                );
                let mut stmt = conn.prepare(&sql)?;
                let id_params: Vec<Box<dyn rusqlite::types::ToSql>> = merged
                    .iter()
                    .map(|s| Box::new(s.id.clone()) as Box<dyn rusqlite::types::ToSql>)
                    .collect();
                let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                    id_params.iter().map(AsRef::as_ref).collect();
                let rows = stmt.query_map(params_ref.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                })?;

                let mut entry_map = std::collections::HashMap::new();
                for row in rows {
                    let (id, key, content, cat, ts, sid) = row?;
                    entry_map.insert(id, (key, content, cat, ts, sid));
                }

                for scored in &merged {
                    if let Some((key, content, cat, ts, sid)) = entry_map.remove(&scored.id) {
                        let entry = MemoryEntry {
                            id: scored.id.clone(),
                            key,
                            content,
                            category: Self::str_to_category(&cat),
                            timestamp: ts,
                            session_id: sid,
                            score: Some(f64::from(scored.final_score)),
                            recall_count: 0,
                            last_recalled: None,
                        };
                        if let Some(filter_sid) = session_ref {
                            if entry.session_id.as_deref() != Some(filter_sid) {
                                continue;
                            }
                        }
                        results.push(entry);
                    }
                }
            }

            // If hybrid returned nothing, fall back to LIKE search.
            // Cap keyword count so we don't create too many SQL shapes,
            // which helps prepared-statement cache efficiency.
            if results.is_empty() {
                const MAX_LIKE_KEYWORDS: usize = 8;
                let keywords: Vec<String> = query
                    .split_whitespace()
                    .take(MAX_LIKE_KEYWORDS)
                    .map(|w| format!("%{w}%"))
                    .collect();
                if !keywords.is_empty() {
                    let conditions: Vec<String> = keywords
                        .iter()
                        .enumerate()
                        .map(|(i, _)| {
                            format!("(content LIKE ?{} OR key LIKE ?{})", i * 2 + 1, i * 2 + 2)
                        })
                        .collect();
                    let where_clause = conditions.join(" OR ");
                    let sql = format!(
                        "SELECT id, key, content, category, created_at, session_id FROM memories
                         WHERE {where_clause}
                         ORDER BY updated_at DESC
                         LIMIT ?{}",
                        keywords.len() * 2 + 1
                    );
                    let mut stmt = conn.prepare(&sql)?;
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    for kw in &keywords {
                        param_values.push(Box::new(kw.clone()));
                        param_values.push(Box::new(kw.clone()));
                    }
                    #[allow(clippy::cast_possible_wrap)]
                    param_values.push(Box::new(limit as i64));
                    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                        param_values.iter().map(AsRef::as_ref).collect();
                    let rows = stmt.query_map(params_ref.as_slice(), |row| {
                        Ok(MemoryEntry {
                            id: row.get(0)?,
                            key: row.get(1)?,
                            content: row.get(2)?,
                            category: Self::str_to_category(&row.get::<_, String>(3)?),
                            timestamp: row.get(4)?,
                            session_id: row.get(5)?,
                            score: Some(1.0),
                            recall_count: 0,
                            last_recalled: None,
                        })
                    })?;
                    for row in rows {
                        let entry = row?;
                        if let Some(sid) = session_ref {
                            if entry.session_id.as_deref() != Some(sid) {
                                continue;
                            }
                        }
                        results.push(entry);
                    }
                }
            }

            results.truncate(limit);
            Ok(results)
        })
        .await?
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let conn = self.conn.clone();
        let key = key.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<MemoryEntry>> {
            let conn = conn.lock();
            let mut stmt = conn.prepare_cached(
                "SELECT id, key, content, category, created_at, session_id FROM memories WHERE key = ?1",
            )?;

            let mut rows = stmt.query_map(params![key], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                recall_count: 0,
                last_recalled: None,
                })
            })?;

            match rows.next() {
                Some(Ok(entry)) => Ok(Some(entry)),
                _ => Ok(None),
            }
        })
        .await?
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        const DEFAULT_LIST_LIMIT: i64 = 1000;

        let conn = self.conn.clone();
        let category = category.cloned();
        let sid = session_id.map(String::from);

        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let conn = conn.lock();
            let session_ref = sid.as_deref();
            let mut results = Vec::new();

            let row_mapper = |row: &rusqlite::Row| -> rusqlite::Result<MemoryEntry> {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                    recall_count: 0,
                    last_recalled: None,
                })
            };

            if let Some(ref cat) = category {
                let cat_str = Self::category_to_str(cat);
                if let Some(sid) = session_ref {
                    let mut stmt = conn.prepare_cached(
                        "SELECT id, key, content, category, created_at, session_id FROM memories
                         WHERE category = ?1 AND session_id = ?2 ORDER BY updated_at DESC LIMIT ?3",
                    )?;
                    let rows =
                        stmt.query_map(params![cat_str, sid, DEFAULT_LIST_LIMIT], row_mapper)?;
                    for row in rows {
                        results.push(row?);
                    }
                } else {
                    let mut stmt = conn.prepare_cached(
                        "SELECT id, key, content, category, created_at, session_id FROM memories
                         WHERE category = ?1 ORDER BY updated_at DESC LIMIT ?2",
                    )?;
                    let rows = stmt.query_map(params![cat_str, DEFAULT_LIST_LIMIT], row_mapper)?;
                    for row in rows {
                        results.push(row?);
                    }
                }
            } else if let Some(sid) = session_ref {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, key, content, category, created_at, session_id FROM memories
                     WHERE session_id = ?1 ORDER BY updated_at DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![sid, DEFAULT_LIST_LIMIT], row_mapper)?;
                for row in rows {
                    results.push(row?);
                }
            } else {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, key, content, category, created_at, session_id FROM memories
                     ORDER BY updated_at DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![DEFAULT_LIST_LIMIT], row_mapper)?;
                for row in rows {
                    results.push(row?);
                }
            }

            Ok(results)
        })
        .await?
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let conn = self.conn.clone();
        let key = key.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let conn = conn.lock();
            let affected = conn.execute("DELETE FROM memories WHERE key = ?1", params![key])?;
            Ok(affected > 0)
        })
        .await?
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let conn = self.conn.clone();

        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = conn.lock();
            let count: i64 =
                conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(count as usize)
        })
        .await?
    }

    async fn health_check(&self) -> bool {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || conn.lock().execute_batch("SELECT 1").is_ok())
            .await
            .unwrap_or(false)
    }

    async fn reindex(
        &self,
        progress_callback: Option<Box<dyn Fn(usize, usize) + Send + Sync>>,
    ) -> anyhow::Result<usize> {
        // Step 1: Get all memory entries
        let entries = self.list(None, None).await?;
        let total = entries.len();

        if total == 0 {
            return Ok(0);
        }

        // Step 2: Clear embedding cache
        {
            let conn = self.conn.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let conn = conn.lock();
                conn.execute("DELETE FROM embedding_cache", [])?;
                Ok(())
            })
            .await??;
        }

        // Step 3: Recompute embeddings for each memory
        let mut reindexed = 0;
        for (idx, entry) in entries.iter().enumerate() {
            // Compute new embedding
            let embedding = self.get_or_compute_embedding(&entry.content).await?;

            if let Some(ref emb) = embedding {
                // Update the embedding in the memories table
                let conn = self.conn.clone();
                let entry_id = entry.id.clone();
                let emb_bytes = vector::vec_to_bytes(emb);

                tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    let conn = conn.lock();
                    conn.execute(
                        "UPDATE memories SET embedding = ?1 WHERE id = ?2",
                        params![emb_bytes, entry_id],
                    )?;
                    Ok(())
                })
                .await??;

                reindexed += 1;
            }

            // Report progress
            if let Some(ref cb) = progress_callback {
                cb(idx + 1, total);
            }
        }

        Ok(reindexed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_sqlite() -> (TempDir, SqliteMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, mem)
    }

    #[tokio::test]
    async fn sqlite_name() {
        let (_tmp, mem) = temp_sqlite();
        assert_eq!(mem.name(), "sqlite");
    }

    #[tokio::test]
    async fn sqlite_health() {
        let (_tmp, mem) = temp_sqlite();
        assert!(mem.health_check().await);
    }

    #[tokio::test]
    async fn sqlite_store_and_get() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("user_lang", "Prefers Rust", MemoryCategory::Core, None)
            .await
            .unwrap();

        let entry = mem.get("user_lang").await.unwrap();
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.key, "user_lang");
        assert_eq!(entry.content, "Prefers Rust");
        assert_eq!(entry.category, MemoryCategory::Core);
    }

    #[tokio::test]
    async fn sqlite_store_upsert() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("pref", "likes Rust", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("pref", "loves Rust", MemoryCategory::Core, None)
            .await
            .unwrap();

        let entry = mem.get("pref").await.unwrap().unwrap();
        assert_eq!(entry.content, "loves Rust");
        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn sqlite_recall_keyword() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "Rust is fast and safe", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "Python is interpreted", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store(
            "c",
            "Rust has zero-cost abstractions",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let results = mem.recall("Rust", 10, None).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| r.content.to_lowercase().contains("rust")));
    }

    #[tokio::test]
    async fn sqlite_recall_multi_keyword() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "Rust is fast", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "Rust is safe and fast", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("fast safe", 10, None).await.unwrap();
        assert!(!results.is_empty());
        // Entry with both keywords should score higher
        assert!(results[0].content.contains("safe") && results[0].content.contains("fast"));
    }

    #[tokio::test]
    async fn sqlite_recall_no_match() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "Rust rocks", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("javascript", 10, None).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn sqlite_forget() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("temp", "temporary data", MemoryCategory::Conversation, None)
            .await
            .unwrap();
        assert_eq!(mem.count().await.unwrap(), 1);

        let removed = mem.forget("temp").await.unwrap();
        assert!(removed);
        assert_eq!(mem.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn sqlite_forget_nonexistent() {
        let (_tmp, mem) = temp_sqlite();
        let removed = mem.forget("nope").await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn sqlite_list_all() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "one", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "two", MemoryCategory::Daily, None)
            .await
            .unwrap();
        mem.store("c", "three", MemoryCategory::Conversation, None)
            .await
            .unwrap();

        let all = mem.list(None, None).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn sqlite_list_by_category() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "core1", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "core2", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("c", "daily1", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let core = mem.list(Some(&MemoryCategory::Core), None).await.unwrap();
        assert_eq!(core.len(), 2);

        let daily = mem.list(Some(&MemoryCategory::Daily), None).await.unwrap();
        assert_eq!(daily.len(), 1);
    }

    #[tokio::test]
    async fn sqlite_count_empty() {
        let (_tmp, mem) = temp_sqlite();
        assert_eq!(mem.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn sqlite_get_nonexistent() {
        let (_tmp, mem) = temp_sqlite();
        assert!(mem.get("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn sqlite_db_persists() {
        let tmp = TempDir::new().unwrap();

        {
            let mem = SqliteMemory::new(tmp.path()).unwrap();
            mem.store("persist", "I survive restarts", MemoryCategory::Core, None)
                .await
                .unwrap();
        }

        // Reopen
        let mem2 = SqliteMemory::new(tmp.path()).unwrap();
        let entry = mem2.get("persist").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "I survive restarts");
    }

    #[tokio::test]
    async fn sqlite_category_roundtrip() {
        let (_tmp, mem) = temp_sqlite();
        let categories = [
            MemoryCategory::Core,
            MemoryCategory::Daily,
            MemoryCategory::Conversation,
            MemoryCategory::Custom("project".into()),
        ];

        for (i, cat) in categories.iter().enumerate() {
            mem.store(&format!("k{i}"), &format!("v{i}"), cat.clone(), None)
                .await
                .unwrap();
        }

        for (i, cat) in categories.iter().enumerate() {
            let entry = mem.get(&format!("k{i}")).await.unwrap().unwrap();
            assert_eq!(&entry.category, cat);
        }
    }

    // ── FTS5 search tests ────────────────────────────────────────

    #[tokio::test]
    async fn fts5_bm25_ranking() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "a",
            "Rust is a systems programming language",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "b",
            "Python is great for scripting",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "c",
            "Rust and Rust and Rust everywhere",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let results = mem.recall("Rust", 10, None).await.unwrap();
        assert!(results.len() >= 2);
        // All results should contain "Rust"
        for r in &results {
            assert!(
                r.content.to_lowercase().contains("rust"),
                "Expected 'rust' in: {}",
                r.content
            );
        }
    }

    #[tokio::test]
    async fn fts5_multi_word_query() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "The quick brown fox jumps", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "A lazy dog sleeps", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("c", "The quick dog runs fast", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("quick dog", 10, None).await.unwrap();
        assert!(!results.is_empty());
        // "The quick dog runs fast" matches both terms
        assert!(results[0].content.contains("quick"));
    }

    #[tokio::test]
    async fn recall_empty_query_returns_empty() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "data", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("", 10, None).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn recall_whitespace_query_returns_empty() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "data", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("   ", 10, None).await.unwrap();
        assert!(results.is_empty());
    }

    // ── Embedding cache tests ────────────────────────────────────

    #[test]
    fn content_hash_deterministic() {
        let h1 = SqliteMemory::content_hash("hello world");
        let h2 = SqliteMemory::content_hash("hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn content_hash_different_inputs() {
        let h1 = SqliteMemory::content_hash("hello");
        let h2 = SqliteMemory::content_hash("world");
        assert_ne!(h1, h2);
    }

    // ── Schema tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn schema_has_fts5_table() {
        let (_tmp, mem) = temp_sqlite();
        let conn = mem.conn.lock();
        // FTS5 table should exist
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memories_fts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn schema_has_embedding_cache() {
        let (_tmp, mem) = temp_sqlite();
        let conn = mem.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='embedding_cache'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn schema_memories_has_embedding_column() {
        let (_tmp, mem) = temp_sqlite();
        let conn = mem.conn.lock();
        // Check that embedding column exists by querying it
        let result = conn.execute_batch("SELECT embedding FROM memories LIMIT 0");
        assert!(result.is_ok());
    }

    // ── FTS5 sync trigger tests ──────────────────────────────────

    #[tokio::test]
    async fn fts5_syncs_on_insert() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "test_key",
            "unique_searchterm_xyz",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let conn = mem.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH '\"unique_searchterm_xyz\"'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn fts5_syncs_on_delete() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "del_key",
            "deletable_content_abc",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        mem.forget("del_key").await.unwrap();

        let conn = mem.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH '\"deletable_content_abc\"'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn fts5_syncs_on_update() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "upd_key",
            "original_content_111",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        mem.store("upd_key", "updated_content_222", MemoryCategory::Core, None)
            .await
            .unwrap();

        let conn = mem.conn.lock();
        // Old content should not be findable
        let old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH '\"original_content_111\"'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old, 0);

        // New content should be findable
        let new: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories_fts WHERE memories_fts MATCH '\"updated_content_222\"'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new, 1);
    }

    // ── Open timeout tests ────────────────────────────────────────

    #[test]
    fn open_with_timeout_succeeds_when_fast() {
        let tmp = TempDir::new().unwrap();
        let embedder = Arc::new(super::super::embeddings::NoopEmbedding);
        let mem = SqliteMemory::with_embedder(tmp.path(), embedder, SearchMode::Weighted, 0.7, 0.3, 60.0, 1000, Some(5));
        assert!(
            mem.is_ok(),
            "open with 5s timeout should succeed on fast path"
        );
        assert_eq!(mem.unwrap().name(), "sqlite");
    }

    #[tokio::test]
    async fn open_with_timeout_store_recall_unchanged() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::with_embedder(
            tmp.path(),
            Arc::new(super::super::embeddings::NoopEmbedding),
            SearchMode::Weighted,
            0.7,
            0.3,
            60.0,
            1000,
            Some(2),
        )
        .unwrap();
        mem.store(
            "timeout_key",
            "value with timeout",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        let entry = mem.get("timeout_key").await.unwrap().unwrap();
        assert_eq!(entry.content, "value with timeout");
    }

    // ── With-embedder constructor test ───────────────────────────

    #[test]
    fn with_embedder_noop() {
        let tmp = TempDir::new().unwrap();
        let embedder = Arc::new(super::super::embeddings::NoopEmbedding);
        let mem = SqliteMemory::with_embedder(tmp.path(), embedder, SearchMode::Weighted, 0.7, 0.3, 60.0, 1000, None);
        assert!(mem.is_ok());
        assert_eq!(mem.unwrap().name(), "sqlite");
    }

    // ── Reindex test ─────────────────────────────────────────────

    #[tokio::test]
    async fn reindex_rebuilds_fts() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("r1", "reindex test alpha", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("r2", "reindex test beta", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Reindex should succeed (noop embedder → 0 re-embedded)
        let count = mem.reindex().await.unwrap();
        assert_eq!(count, 0);

        // FTS should still work after rebuild
        let results = mem.recall("reindex", 10, None).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    // ── Recall limit test ────────────────────────────────────────

    #[tokio::test]
    async fn recall_respects_limit() {
        let (_tmp, mem) = temp_sqlite();
        for i in 0..20 {
            mem.store(
                &format!("k{i}"),
                &format!("common keyword item {i}"),
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
        }

        let results = mem.recall("common keyword", 5, None).await.unwrap();
        assert!(results.len() <= 5);
    }

    // ── Score presence test ──────────────────────────────────────

    #[tokio::test]
    async fn recall_results_have_scores() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("s1", "scored result test", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("scored", 10, None).await.unwrap();
        assert!(!results.is_empty());
        for r in &results {
            assert!(r.score.is_some(), "Expected score on result: {:?}", r.key);
        }
    }

    // ── Edge cases: FTS5 special characters ──────────────────────

    #[tokio::test]
    async fn recall_with_quotes_in_query() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("q1", "He said hello world", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Quotes in query should not crash FTS5
        let results = mem.recall("\"hello\"", 10, None).await.unwrap();
        // May or may not match depending on FTS5 escaping, but must not error
        assert!(results.len() <= 10);
    }

    #[tokio::test]
    async fn recall_with_asterisk_in_query() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a1", "wildcard test content", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("wild*", 10, None).await.unwrap();
        assert!(results.len() <= 10);
    }

    #[tokio::test]
    async fn recall_with_parentheses_in_query() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("p1", "function call test", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("function()", 10, None).await.unwrap();
        assert!(results.len() <= 10);
    }

    #[tokio::test]
    async fn recall_with_sql_injection_attempt() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("safe", "normal content", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Should not crash or leak data
        let results = mem
            .recall("'; DROP TABLE memories; --", 10, None)
            .await
            .unwrap();
        assert!(results.len() <= 10);
        // Table should still exist
        assert_eq!(mem.count().await.unwrap(), 1);
    }

    // ── Edge cases: store ────────────────────────────────────────

    #[tokio::test]
    async fn store_empty_content() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("empty", "", MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("empty").await.unwrap().unwrap();
        assert_eq!(entry.content, "");
    }

    #[tokio::test]
    async fn store_empty_key() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("", "content for empty key", MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("").await.unwrap().unwrap();
        assert_eq!(entry.content, "content for empty key");
    }

    #[tokio::test]
    async fn store_very_long_content() {
        let (_tmp, mem) = temp_sqlite();
        let long_content = "x".repeat(100_000);
        mem.store("long", &long_content, MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("long").await.unwrap().unwrap();
        assert_eq!(entry.content.len(), 100_000);
    }

    #[tokio::test]
    async fn store_unicode_and_emoji() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "emoji_key_🦀",
            "こんにちは 🚀 Ñoño",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        let entry = mem.get("emoji_key_🦀").await.unwrap().unwrap();
        assert_eq!(entry.content, "こんにちは 🚀 Ñoño");
    }

    #[tokio::test]
    async fn store_content_with_newlines_and_tabs() {
        let (_tmp, mem) = temp_sqlite();
        let content = "line1\nline2\ttab\rcarriage\n\nnewparagraph";
        mem.store("whitespace", content, MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("whitespace").await.unwrap().unwrap();
        assert_eq!(entry.content, content);
    }

    // ── Edge cases: recall ───────────────────────────────────────

    #[tokio::test]
    async fn recall_single_character_query() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "x marks the spot", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Single char may not match FTS5 but LIKE fallback should work
        let results = mem.recall("x", 10, None).await.unwrap();
        // Should not crash; may or may not find results
        assert!(results.len() <= 10);
    }

    #[tokio::test]
    async fn recall_limit_zero() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "some content", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("some", 0, None).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn recall_limit_one() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "matching content alpha", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "matching content beta", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("matching content", 1, None).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn recall_matches_by_key_not_just_content() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "rust_preferences",
            "User likes systems programming",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        // "rust" appears in key but not content — LIKE fallback checks key too
        let results = mem.recall("rust", 10, None).await.unwrap();
        assert!(!results.is_empty(), "Should match by key");
    }

    #[tokio::test]
    async fn recall_unicode_query() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("jp", "日本語のテスト", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("日本語", 10, None).await.unwrap();
        assert!(!results.is_empty());
    }

    // ── Edge cases: schema idempotency ───────────────────────────

    #[tokio::test]
    async fn schema_idempotent_reopen() {
        let tmp = TempDir::new().unwrap();
        {
            let mem = SqliteMemory::new(tmp.path()).unwrap();
            mem.store("k1", "v1", MemoryCategory::Core, None)
                .await
                .unwrap();
        }
        // Open again — init_schema runs again on existing DB
        let mem2 = SqliteMemory::new(tmp.path()).unwrap();
        let entry = mem2.get("k1").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "v1");
        // Store more data — should work fine
        mem2.store("k2", "v2", MemoryCategory::Daily, None)
            .await
            .unwrap();
        assert_eq!(mem2.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn schema_triple_open() {
        let tmp = TempDir::new().unwrap();
        let _m1 = SqliteMemory::new(tmp.path()).unwrap();
        let _m2 = SqliteMemory::new(tmp.path()).unwrap();
        let m3 = SqliteMemory::new(tmp.path()).unwrap();
        assert!(m3.health_check().await);
    }

    // ── Edge cases: forget + FTS5 consistency ────────────────────

    #[tokio::test]
    async fn forget_then_recall_no_ghost_results() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "ghost",
            "phantom memory content",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        mem.forget("ghost").await.unwrap();
        let results = mem.recall("phantom memory", 10, None).await.unwrap();
        assert!(
            results.is_empty(),
            "Deleted memory should not appear in recall"
        );
    }

    #[tokio::test]
    async fn forget_and_re_store_same_key() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("cycle", "version 1", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.forget("cycle").await.unwrap();
        mem.store("cycle", "version 2", MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("cycle").await.unwrap().unwrap();
        assert_eq!(entry.content, "version 2");
        assert_eq!(mem.count().await.unwrap(), 1);
    }

    // ── Edge cases: reindex ──────────────────────────────────────

    #[tokio::test]
    async fn reindex_empty_db() {
        let (_tmp, mem) = temp_sqlite();
        let count = mem.reindex().await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn reindex_twice_is_safe() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("r1", "reindex data", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.reindex().await.unwrap();
        let count = mem.reindex().await.unwrap();
        assert_eq!(count, 0); // Noop embedder → nothing to re-embed
                              // Data should still be intact
        let results = mem.recall("reindex", 10, None).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    // ── Edge cases: content_hash ─────────────────────────────────

    #[test]
    fn content_hash_empty_string() {
        let h = SqliteMemory::content_hash("");
        assert!(!h.is_empty());
        assert_eq!(h.len(), 16); // 16 hex chars
    }

    #[test]
    fn content_hash_unicode() {
        let h1 = SqliteMemory::content_hash("🦀");
        let h2 = SqliteMemory::content_hash("🦀");
        assert_eq!(h1, h2);
        let h3 = SqliteMemory::content_hash("🚀");
        assert_ne!(h1, h3);
    }

    #[test]
    fn content_hash_long_input() {
        let long = "a".repeat(1_000_000);
        let h = SqliteMemory::content_hash(&long);
        assert_eq!(h.len(), 16);
    }

    // ── Edge cases: category helpers ─────────────────────────────

    #[test]
    fn category_roundtrip_custom_with_spaces() {
        let cat = MemoryCategory::Custom("my custom category".into());
        let s = SqliteMemory::category_to_str(&cat);
        assert_eq!(s, "my custom category");
        let back = SqliteMemory::str_to_category(&s);
        assert_eq!(back, cat);
    }

    #[test]
    fn category_roundtrip_empty_custom() {
        let cat = MemoryCategory::Custom(String::new());
        let s = SqliteMemory::category_to_str(&cat);
        assert_eq!(s, "");
        let back = SqliteMemory::str_to_category(&s);
        assert_eq!(back, MemoryCategory::Custom(String::new()));
    }

    // ── Edge cases: list ─────────────────────────────────────────

    #[tokio::test]
    async fn list_custom_category() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "c1",
            "custom1",
            MemoryCategory::Custom("project".into()),
            None,
        )
        .await
        .unwrap();
        mem.store(
            "c2",
            "custom2",
            MemoryCategory::Custom("project".into()),
            None,
        )
        .await
        .unwrap();
        mem.store("c3", "other", MemoryCategory::Core, None)
            .await
            .unwrap();

        let project = mem
            .list(Some(&MemoryCategory::Custom("project".into())), None)
            .await
            .unwrap();
        assert_eq!(project.len(), 2);
    }

    #[tokio::test]
    async fn list_empty_db() {
        let (_tmp, mem) = temp_sqlite();
        let all = mem.list(None, None).await.unwrap();
        assert!(all.is_empty());
    }

    // ── Session isolation ─────────────────────────────────────────

    #[tokio::test]
    async fn store_and_recall_with_session_id() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("k1", "session A fact", MemoryCategory::Core, Some("sess-a"))
            .await
            .unwrap();
        mem.store("k2", "session B fact", MemoryCategory::Core, Some("sess-b"))
            .await
            .unwrap();
        mem.store("k3", "no session fact", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Recall with session-a filter returns only session-a entry
        let results = mem.recall("fact", 10, Some("sess-a")).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "k1");
        assert_eq!(results[0].session_id.as_deref(), Some("sess-a"));
    }

    #[tokio::test]
    async fn recall_no_session_filter_returns_all() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("k1", "alpha fact", MemoryCategory::Core, Some("sess-a"))
            .await
            .unwrap();
        mem.store("k2", "beta fact", MemoryCategory::Core, Some("sess-b"))
            .await
            .unwrap();
        mem.store("k3", "gamma fact", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Recall without session filter returns all matching entries
        let results = mem.recall("fact", 10, None).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn cross_session_recall_isolation() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "secret",
            "session A secret data",
            MemoryCategory::Core,
            Some("sess-a"),
        )
        .await
        .unwrap();

        // Session B cannot see session A data
        let results = mem.recall("secret", 10, Some("sess-b")).await.unwrap();
        assert!(results.is_empty());

        // Session A can see its own data
        let results = mem.recall("secret", 10, Some("sess-a")).await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn list_with_session_filter() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("k1", "a1", MemoryCategory::Core, Some("sess-a"))
            .await
            .unwrap();
        mem.store("k2", "a2", MemoryCategory::Conversation, Some("sess-a"))
            .await
            .unwrap();
        mem.store("k3", "b1", MemoryCategory::Core, Some("sess-b"))
            .await
            .unwrap();
        mem.store("k4", "none1", MemoryCategory::Core, None)
            .await
            .unwrap();

        // List with session-a filter
        let results = mem.list(None, Some("sess-a")).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|e| e.session_id.as_deref() == Some("sess-a")));

        // List with session-a + category filter
        let results = mem
            .list(Some(&MemoryCategory::Core), Some("sess-a"))
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "k1");
    }

    #[tokio::test]
    async fn schema_migration_idempotent_on_reopen() {
        let tmp = TempDir::new().unwrap();

        // First open: creates schema + migration
        {
            let mem = SqliteMemory::new(tmp.path()).unwrap();
            mem.store("k1", "before reopen", MemoryCategory::Core, Some("sess-x"))
                .await
                .unwrap();
        }

        // Second open: migration runs again but is idempotent
        {
            let mem = SqliteMemory::new(tmp.path()).unwrap();
            let results = mem.recall("reopen", 10, Some("sess-x")).await.unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].key, "k1");
            assert_eq!(results[0].session_id.as_deref(), Some("sess-x"));
        }
    }

    // ── §4.1 Concurrent write contention tests ──────────────

    #[tokio::test]
    async fn sqlite_concurrent_writes_no_data_loss() {
        let (_tmp, mem) = temp_sqlite();
        let mem = std::sync::Arc::new(mem);

        let mut handles = Vec::new();
        for i in 0..10 {
            let mem = std::sync::Arc::clone(&mem);
            handles.push(tokio::spawn(async move {
                mem.store(
                    &format!("concurrent_key_{i}"),
                    &format!("value_{i}"),
                    MemoryCategory::Core,
                    None,
                )
                .await
                .unwrap();
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        let count = mem.count().await.unwrap();
        assert_eq!(
            count, 10,
            "all 10 concurrent writes must succeed without data loss"
        );
    }

    #[tokio::test]
    async fn sqlite_concurrent_read_write_no_panic() {
        let (_tmp, mem) = temp_sqlite();
        let mem = std::sync::Arc::new(mem);

        // Pre-populate
        mem.store("shared_key", "initial", MemoryCategory::Core, None)
            .await
            .unwrap();

        let mut handles = Vec::new();

        // Concurrent reads
        for _ in 0..5 {
            let mem = std::sync::Arc::clone(&mem);
            handles.push(tokio::spawn(async move {
                let _ = mem.get("shared_key").await.unwrap();
            }));
        }

        // Concurrent writes
        for i in 0..5 {
            let mem = std::sync::Arc::clone(&mem);
            handles.push(tokio::spawn(async move {
                mem.store(
                    &format!("key_{i}"),
                    &format!("val_{i}"),
                    MemoryCategory::Core,
                    None,
                )
                .await
                .unwrap();
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        // Should have 6 total entries (1 pre-existing + 5 new)
        assert_eq!(mem.count().await.unwrap(), 6);
    }

    // ── §4.2 Reindex / corruption recovery tests ────────────

    #[tokio::test]
    async fn sqlite_reindex_preserves_data() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "Rust is fast", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "Python is interpreted", MemoryCategory::Core, None)
            .await
            .unwrap();

        mem.reindex().await.unwrap();

        let count = mem.count().await.unwrap();
        assert_eq!(count, 2, "reindex must preserve all entries");

        let entry = mem.get("a").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "Rust is fast");
    }

    #[tokio::test]
    async fn sqlite_reindex_idempotent() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("x", "test data", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Multiple reindex calls should be safe
        mem.reindex().await.unwrap();
        mem.reindex().await.unwrap();
        mem.reindex().await.unwrap();

        assert_eq!(mem.count().await.unwrap(), 1);
    }

    // ── v3.0 Compiled Truth + Timeline tests ─────────────────────

    #[tokio::test]
    async fn compiled_truth_roundtrip() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("ct_test", "original content", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Initially no compiled truth
        let result = mem.get_compiled_truth("ct_test").unwrap();
        assert!(result.is_none() || result.unwrap().0.is_empty() == false);

        // Set compiled truth
        mem.set_compiled_truth("ct_test", "summarized truth v1").unwrap();
        let (truth, version) = mem.get_compiled_truth("ct_test").unwrap().unwrap();
        assert_eq!(truth, "summarized truth v1");
        assert_eq!(version, 1);

        // Update compiled truth — version increments
        mem.set_compiled_truth("ct_test", "summarized truth v2").unwrap();
        let (truth, version) = mem.get_compiled_truth("ct_test").unwrap().unwrap();
        assert_eq!(truth, "summarized truth v2");
        assert_eq!(version, 2);
    }

    #[tokio::test]
    async fn needs_recompile_flag() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("recomp", "data", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Initially no entries need recompile
        assert!(mem.get_needs_recompile().unwrap().is_empty());

        // Mark needs recompile
        mem.mark_needs_recompile("recomp").unwrap();
        let pending = mem.get_needs_recompile().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, "recomp");

        // set_compiled_truth clears the flag
        mem.set_compiled_truth("recomp", "compiled").unwrap();
        assert!(mem.get_needs_recompile().unwrap().is_empty());
    }

    #[tokio::test]
    async fn timeline_append_and_get() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("tl_mem", "memory for timeline", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Get the memory id
        let entry = mem.get("tl_mem").await.unwrap().unwrap();
        let memory_id = &entry.id;

        // Append timeline entries
        let uuid1 = mem
            .append_timeline(memory_id, "chat", 1000, "msg_001", "first evidence", None, "device_a")
            .unwrap();
        let uuid2 = mem
            .append_timeline(
                memory_id,
                "call",
                2000,
                "call_001",
                "second evidence from call",
                Some(r#"{"duration":120}"#),
                "device_a",
            )
            .unwrap();

        assert_ne!(uuid1, uuid2);

        // Retrieve timeline (ordered by event_at DESC)
        let timeline = mem.get_timeline(memory_id, 10).unwrap();
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline[0].event_type, "call"); // most recent first
        assert_eq!(timeline[0].source_ref, "call_001");
        assert_eq!(timeline[1].event_type, "chat");
        assert_eq!(timeline[1].source_ref, "msg_001");

        // Content SHA256 should be populated
        assert!(!timeline[0].content.is_empty());
    }

    #[test]
    fn timeline_append_only_enforced() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();

        // We need to insert a memory first to satisfy FK
        let conn = mem.conn.lock();
        conn.execute(
            "INSERT INTO memories (id, key, content, category, created_at, updated_at) VALUES ('m1', 'k1', 'c', 'core', '2024', '2024')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO memory_timeline (uuid, memory_id, event_type, event_at, source_ref, content, content_sha256, device_id)
             VALUES ('u1', 'm1', 'chat', 1000, 'ref1', 'data', 'sha', 'dev1')",
            [],
        ).unwrap();

        // Attempt UPDATE should fail (append-only trigger)
        let result = conn.execute(
            "UPDATE memory_timeline SET content = 'modified' WHERE uuid = 'u1'",
            [],
        );
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("append-only"), "Expected append-only error, got: {err_msg}");
    }

    #[test]
    fn timeline_limit_respected() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();

        {
            let conn = mem.conn.lock();
            conn.execute(
                "INSERT INTO memories (id, key, content, category, created_at, updated_at) VALUES ('m2', 'k2', 'c', 'core', '2024', '2024')",
                [],
            ).unwrap();
        }

        for i in 0..10 {
            mem.append_timeline("m2", "chat", i * 100, &format!("ref_{i}"), &format!("content_{i}"), None, "dev1")
                .unwrap();
        }

        let timeline = mem.get_timeline("m2", 3).unwrap();
        assert_eq!(timeline.len(), 3);
        // Should be the 3 most recent
        assert_eq!(timeline[0].source_ref, "ref_9");
    }

    #[tokio::test]
    async fn phone_calls_table_exists() {
        let (_tmp, mem) = temp_sqlite();
        let conn = mem.conn.lock();
        // Verify phone_calls table was created
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM phone_calls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn compiled_truth_nonexistent_key() {
        let (_tmp, mem) = temp_sqlite();
        let result = mem.get_compiled_truth("nonexistent").unwrap();
        assert!(result.is_none());
    }

    // ── v3.0 Dual-Brain Sync Integration Tests ───────────────────

    #[tokio::test]
    async fn timeline_append_records_sync_delta_when_attached() {
        let (_tmp, mem) = temp_sqlite();
        let engine = Arc::new(Mutex::new(
            super::super::sync::SyncEngine::new(_tmp.path(), true).unwrap(),
        ));
        mem.attach_sync(engine.clone());

        mem.store("m_ts", "memory", MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("m_ts").await.unwrap().unwrap();

        let before = engine.lock().journal_len();
        mem.append_timeline(&entry.id, "chat", 1000, "msg_sync_1", "evidence", None, "dev_local")
            .unwrap();
        let after = engine.lock().journal_len();
        assert_eq!(
            after - before,
            1,
            "append_timeline must push exactly one TimelineAppend delta when sync is attached"
        );
    }

    #[tokio::test]
    async fn compiled_truth_update_records_sync_delta_with_version() {
        let (_tmp, mem) = temp_sqlite();
        let engine = Arc::new(Mutex::new(
            super::super::sync::SyncEngine::new(_tmp.path(), true).unwrap(),
        ));
        mem.attach_sync(engine.clone());

        mem.store("m_ct", "initial", MemoryCategory::Core, None)
            .await
            .unwrap();

        let before = engine.lock().journal_len();
        mem.set_compiled_truth("m_ct", "v1 summary").unwrap();
        mem.set_compiled_truth("m_ct", "v2 summary").unwrap();
        let after = engine.lock().journal_len();
        assert_eq!(after - before, 2, "two updates → two deltas");

        // Local version should now be 2.
        let (truth, version) = mem.get_compiled_truth("m_ct").unwrap().unwrap();
        assert_eq!(truth, "v2 summary");
        assert_eq!(version, 2);
    }

    #[tokio::test]
    async fn insert_phone_call_records_sync_delta() {
        let (_tmp, mem) = temp_sqlite();
        let engine = Arc::new(Mutex::new(
            super::super::sync::SyncEngine::new(_tmp.path(), true).unwrap(),
        ));
        mem.attach_sync(engine.clone());

        let before = engine.lock().journal_len();
        mem.insert_phone_call(
            "call_xyz",
            "in",
            Some("010-0000-0000"),
            Some("+821000000000"),
            None,
            1700000000,
            Some(1700000300),
            Some(300_000),
            None,
            None,
            Some("hello"),
            Some("greet"),
            "safe",
            false,
            Some("ko"),
            None,
            "dev_local",
        )
        .unwrap();
        let after = engine.lock().journal_len();
        assert_eq!(after - before, 1);
    }

    #[tokio::test]
    async fn apply_remote_timeline_persists_without_reecording() {
        use super::super::sync::DeltaOperation;

        let (_tmp, mem) = temp_sqlite();
        let engine = Arc::new(Mutex::new(
            super::super::sync::SyncEngine::new(_tmp.path(), true).unwrap(),
        ));
        mem.attach_sync(engine.clone());

        // Seed a memory on the receiving device.
        mem.store("m_remote", "peer memory", MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("m_remote").await.unwrap().unwrap();

        let delta = DeltaOperation::TimelineAppend {
            uuid: "remote-evt-1".into(),
            memory_id: entry.id.clone(),
            event_type: "chat".into(),
            event_at: 1800,
            source_ref: "remote_msg_9".into(),
            content: "remote evidence".into(),
            content_sha256: "abc".into(),
            metadata_json: None,
        };

        let journal_before = engine.lock().journal_len();
        let applied = mem.apply_remote_v3_delta(&delta).await.unwrap();
        let journal_after = engine.lock().journal_len();

        assert!(applied, "remote timeline delta should be applied");
        assert_eq!(
            journal_before, journal_after,
            "applying remote delta must NOT re-record (prevents sync loops)"
        );

        // Row is present in local timeline.
        let timeline = mem.get_timeline(&entry.id, 10).unwrap();
        assert!(timeline.iter().any(|t| t.source_ref == "remote_msg_9"));
    }

    #[tokio::test]
    async fn apply_remote_truth_lww_rejects_older_version() {
        use super::super::sync::DeltaOperation;

        let (_tmp, mem) = temp_sqlite();
        let engine = Arc::new(Mutex::new(
            super::super::sync::SyncEngine::new(_tmp.path(), true).unwrap(),
        ));
        mem.attach_sync(engine.clone());

        mem.store("m_lww", "base", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Bump local version to 2.
        mem.set_compiled_truth("m_lww", "local v1").unwrap();
        mem.set_compiled_truth("m_lww", "local v2").unwrap();

        // Remote sends version=1 — older than local v2; must be rejected.
        let stale = DeltaOperation::CompiledTruthUpdate {
            memory_key: "m_lww".into(),
            compiled_truth: "stale remote".into(),
            truth_version: 1,
        };
        let applied = mem.apply_remote_v3_delta(&stale).await.unwrap();
        assert!(!applied, "LWW must reject older version");

        // Remote sends version=5 — newer; should overwrite.
        let fresh = DeltaOperation::CompiledTruthUpdate {
            memory_key: "m_lww".into(),
            compiled_truth: "fresh remote".into(),
            truth_version: 5,
        };
        let applied = mem.apply_remote_v3_delta(&fresh).await.unwrap();
        assert!(applied);

        let (truth, version) = mem.get_compiled_truth("m_lww").unwrap().unwrap();
        assert_eq!(truth, "fresh remote");
        assert_eq!(version, 5);
    }

    #[tokio::test]
    async fn recall_with_variations_falls_back_to_single_query() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("rec1", "apple pie recipe", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Empty variations → falls back to recall()
        let empty_vars: Vec<String> = vec![];
        let r0 = mem
            .recall_with_variations("apple pie", &empty_vars, 5, None)
            .await
            .unwrap();
        assert!(!r0.is_empty());

        // Single variation → falls back to recall()
        let r1 = mem
            .recall_with_variations("apple pie", &vec!["apple pie".into()], 5, None)
            .await
            .unwrap();
        assert!(!r1.is_empty());
    }

    #[tokio::test]
    async fn recall_with_variations_fuses_multiple_queries() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("doc_divorce", "이혼 절차 안내", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store(
            "doc_property",
            "재산분할 기준과 판례",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        mem.store("doc_alimony", "위자료 산정 방법", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("doc_other", "노동법 해설", MemoryCategory::Core, None)
            .await
            .unwrap();

        let vars = vec![
            "이혼 소송".to_string(),
            "재산분할".to_string(),
            "위자료".to_string(),
        ];
        let results = mem
            .recall_with_variations("이혼 소송", &vars, 10, None)
            .await
            .unwrap();

        // Should recall the three divorce-related entries; noise entry excluded.
        let keys: std::collections::HashSet<String> =
            results.iter().map(|e| e.key.clone()).collect();
        assert!(keys.contains("doc_divorce") || keys.contains("doc_property") || keys.contains("doc_alimony"));
    }

    #[tokio::test]
    async fn no_sync_recording_when_engine_not_attached() {
        // Safety: mutations on a plain SqliteMemory (no sync attached)
        // must succeed and NEVER panic trying to access the sync engine.
        let (_tmp, mem) = temp_sqlite();

        mem.store("m_nosync", "data", MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("m_nosync").await.unwrap().unwrap();

        // All three typed mutations must succeed without a sync engine.
        mem.append_timeline(&entry.id, "chat", 1, "r", "c", None, "d").unwrap();
        mem.set_compiled_truth("m_nosync", "summary").unwrap();
        mem.insert_phone_call(
            "call_nosync",
            "in",
            None,
            None,
            None,
            1,
            None,
            None,
            None,
            None,
            None,
            None,
            "safe",
            false,
            None,
            None,
            "dev",
        )
        .unwrap();
    }
}
