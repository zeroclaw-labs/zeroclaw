use super::embeddings::EmbeddingProvider;
use super::traits::{ExportFilter, Memory, MemoryCategory, MemoryEntry, is_recent_recall_query};
use super::vector;
use anyhow::Context;
use async_trait::async_trait;
use chrono::Local;
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::sync::{Mutex as StdMutex, MutexGuard};
use std::thread;
use std::time::Duration;
use uuid::Uuid;
use zeroclaw_api::session_keys::sanitize_session_key;
use zeroclaw_config::schema::SearchMode;

/// Maximum allowed open timeout (seconds) to avoid unreasonable waits.
const SQLITE_OPEN_TIMEOUT_CAP_SECS: u64 = 300;
static SQLITE_MEMORY_STARTUP_LOCK: StdMutex<()> = StdMutex::new(());

fn acquire_sqlite_startup_lock() -> MutexGuard<'static, ()> {
    SQLITE_MEMORY_STARTUP_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// SQLite-backed persistent memory — the brain
///
/// Full-stack search engine:
/// - **Vector DB**: embeddings stored as BLOB, cosine similarity search
/// - **Keyword Search**: FTS5 virtual table with BM25 scoring
/// - **Hybrid Merge**: weighted fusion of vector + keyword results
/// - **Embedding Cache**: LRU-evicted cache to avoid redundant API calls
/// - **Safe Reindex**: temp DB → seed → sync → atomic swap → rollback
pub struct SqliteMemory {
    alias: String,
    conn: Arc<Mutex<Connection>>,
    embedder: Arc<dyn EmbeddingProvider>,
    vector_weight: f32,
    keyword_weight: f32,
    cache_max: usize,
    search_mode: SearchMode,
}

impl SqliteMemory {
    pub fn new(alias: &str, workspace_dir: &Path) -> anyhow::Result<Self> {
        Self::with_embedder(
            alias,
            workspace_dir,
            Arc::new(super::embeddings::NoopEmbedding),
            0.7,
            0.3,
            10_000,
            None,
            SearchMode::default(),
        )
    }

    /// Like `new`, but stores data in `{db_name}.db` instead of `brain.db`.
    pub fn new_named(alias: &str, workspace_dir: &Path, db_name: &str) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("memory").join(format!("{db_name}.db"));
        let _startup_guard = acquire_sqlite_startup_lock();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Self::open_connection(&db_path, None)?;
        conn.execute_batch(
            // foreign_keys is OFF by default in SQLite and is a
            // per-connection PRAGMA, so the multi-agent migration's
            // `REFERENCES agents(id)` constraint would be unenforced
            // without this. Set it before any writes flow through.
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA mmap_size    = 8388608;
             PRAGMA cache_size   = -2000;
             PRAGMA temp_store   = MEMORY;",
        )?;
        Self::init_schema(&conn)?;
        zeroclaw_config::schema::v2::migrate_sqlite_memory_to_v3(&db_path, &conn)?;
        Ok(Self {
            alias: alias.to_string(),
            conn: Arc::new(Mutex::new(conn)),
            embedder: Arc::new(super::embeddings::NoopEmbedding),
            vector_weight: 0.7,
            keyword_weight: 0.3,
            cache_max: 10_000,
            search_mode: SearchMode::default(),
        })
    }

    /// Build SQLite memory with optional open timeout.
    ///
    /// If `open_timeout_secs` is `Some(n)`, opening the database is limited to `n` seconds
    /// (capped at 300). Useful when the DB file may be locked or on slow storage.
    /// `None` = wait indefinitely (default).
    pub fn with_embedder(
        alias: &str,
        workspace_dir: &Path,
        embedder: Arc<dyn EmbeddingProvider>,
        vector_weight: f32,
        keyword_weight: f32,
        cache_max: usize,
        open_timeout_secs: Option<u64>,
        search_mode: SearchMode,
    ) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("memory").join("brain.db");
        let _startup_guard = acquire_sqlite_startup_lock();

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Self::open_connection(&db_path, open_timeout_secs)?;

        // ── Production-grade PRAGMA tuning ──────────────────────
        // foreign_keys ON: SQLite defaults FKs OFF per-connection;
        //                  the multi-agent migration's REFERENCES
        //                  agents(id) is unenforced without it.
        // WAL mode: concurrent reads during writes, crash-safe
        // normal sync: 2× write speed, still durable on WAL
        // mmap 8 MB: let the OS page-cache serve hot reads
        // cache 2 MB: keep ~500 hot pages in-process
        // temp_store memory: temp tables never hit disk
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA mmap_size    = 8388608;
             PRAGMA cache_size   = -2000;
             PRAGMA temp_store   = MEMORY;",
        )?;

        Self::init_schema(&conn)?;
        zeroclaw_config::schema::v2::migrate_sqlite_memory_to_v3(&db_path, &conn)?;

        Ok(Self {
            alias: alias.to_string(),
            conn: Arc::new(Mutex::new(conn)),
            embedder,
            vector_weight,
            keyword_weight,
            cache_max,
            search_mode,
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
        fn is_db_locked_error(e: &rusqlite::Error) -> bool {
            use rusqlite::ffi::ErrorCode;
            matches!(
                e,
                rusqlite::Error::SqliteFailure(err, _)
                    if matches!(err.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
            )
        }

        fn execute_batch_retry(conn: &Connection, sql: &str) -> Result<(), rusqlite::Error> {
            // SQLite can return "database is locked" during concurrent schema
            // initialization even though the operations are safe/idempotent.
            // Retry briefly instead of failing startup.
            let mut backoff = Duration::from_millis(10);
            let max_backoff = Duration::from_millis(250);
            let max_attempts: usize = 24; // Worst-case sleep is ~4.8s.

            for attempt in 1..=max_attempts {
                match conn.execute_batch(sql) {
                    Ok(()) => return Ok(()),
                    Err(e) if is_db_locked_error(&e) && attempt < max_attempts => {
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(max_backoff);
                    }
                    Err(e) => return Err(e),
                }
            }

            // Unreachable due to early-return above, but keep control-flow explicit.
            Ok(())
        }

        fn memories_has_column(conn: &Connection, name: &str) -> anyhow::Result<bool> {
            let mut stmt = conn.prepare("PRAGMA table_info(memories)")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let col_name: String = row.get(1)?;
                if col_name == name {
                    return Ok(true);
                }
            }
            Ok(false)
        }

        fn is_duplicate_column_error(e: &rusqlite::Error) -> bool {
            matches!(
                e,
                rusqlite::Error::SqliteFailure(_, Some(msg)) if msg.contains("duplicate column name")
            )
        }

        fn add_memories_column_if_missing(
            conn: &Connection,
            name: &str,
            alter_sql: &str,
        ) -> anyhow::Result<()> {
            if memories_has_column(conn, name)? {
                return Ok(());
            }

            match execute_batch_retry(conn, alter_sql) {
                Ok(()) => Ok(()),
                Err(e) if is_duplicate_column_error(&e) => Ok(()),
                Err(e) => Err(e)
                    .with_context(|| format!("SQLite migration failed adding memories.{name}")),
            }
        }

        execute_batch_retry(
            conn,
            "-- Core memories table. This is an intermediate shape; the V3
            -- migration in `zeroclaw_config::schema::v2::migrate_sqlite_memory_to_v3`
            -- rebuilds it with the `agent_id` column and a composite
            -- `UNIQUE (agent_id, key)` constraint immediately after init.
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
            CREATE INDEX IF NOT EXISTS idx_memories_key ON memories(key);

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
        )
        .with_context(|| "SQLite init_schema failed: CREATE base schema")?;

        add_memories_column_if_missing(
            conn,
            "session_id",
            "ALTER TABLE memories ADD COLUMN session_id TEXT;",
        )?;
        execute_batch_retry(
            conn,
            "CREATE INDEX IF NOT EXISTS idx_memories_session ON memories(session_id);",
        )
        .with_context(|| "SQLite init_schema failed: CREATE INDEX idx_memories_session")?;

        add_memories_column_if_missing(
            conn,
            "namespace",
            "ALTER TABLE memories ADD COLUMN namespace TEXT DEFAULT 'default';",
        )?;
        execute_batch_retry(
            conn,
            "CREATE INDEX IF NOT EXISTS idx_memories_namespace ON memories(namespace);",
        )
        .with_context(|| "SQLite init_schema failed: CREATE INDEX idx_memories_namespace")?;

        add_memories_column_if_missing(
            conn,
            "importance",
            "ALTER TABLE memories ADD COLUMN importance REAL DEFAULT 0.5;",
        )?;

        add_memories_column_if_missing(
            conn,
            "superseded_by",
            "ALTER TABLE memories ADD COLUMN superseded_by TEXT;",
        )?;

        Self::migrate_session_ids_to_sanitized(conn)?;

        Ok(())
    }

    /// One-shot, idempotent normalization of `memories.session_id`.
    ///
    /// The orchestrator sanitizes session keys at the source so the runtime
    /// HashMap, on-disk JSONL filename, and `session_id` filter for recall
    /// all agree. Rows written before that fix retained the raw, un-sanitized
    /// form (e.g. `slack_C123_1.2_user one`) and would be invisible to the
    /// new sanitized recall filter. Rewrite them once at startup; later runs
    /// find nothing to update because `sanitize_session_key` is idempotent.
    fn migrate_session_ids_to_sanitized(conn: &Connection) -> anyhow::Result<()> {
        let distinct: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT DISTINCT session_id FROM memories WHERE session_id IS NOT NULL")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut update =
            conn.prepare("UPDATE memories SET session_id = ?1 WHERE session_id = ?2")?;
        let mut rewritten = 0usize;
        for old in &distinct {
            let new = sanitize_session_key(old);
            if new != *old {
                update.execute(params![new, old])?;
                rewritten += 1;
            }
        }

        if rewritten > 0 {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"rewritten": rewritten})),
                "Normalized session_id values in memories table to sanitized form"
            );
        }

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

    /// Provide access to the connection for advanced queries (e.g. retrieval pipeline).
    pub fn connection(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }

    /// Get embedding from cache, or compute + cache it
    pub async fn get_or_compute_embedding(&self, text: &str) -> anyhow::Result<Option<Vec<f32>>> {
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
            let mut stmt =
                conn.prepare("SELECT embedding FROM embedding_cache WHERE content_hash = ?1")?;
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
    pub fn fts5_search(
        conn: &Connection,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        // Escape FTS5 special chars and build query
        let fts_query: String = query
            .split_whitespace()
            .map(Self::fts5_term_query)
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

        let mut stmt = conn.prepare(sql)?;
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

    fn fts5_term_query(term: &str) -> String {
        if let Some(prefix) = term.strip_suffix('*')
            && !prefix.is_empty()
        {
            let escaped = prefix.replace('"', "\"\"");
            format!("\"{escaped}\"*")
        } else {
            let escaped = term.replace('"', "\"\"");
            format!("\"{escaped}\"")
        }
    }

    fn like_search_pattern(term: &str) -> String {
        if let Some(prefix) = term.strip_suffix('*')
            && !prefix.is_empty()
        {
            return format!("%{}%", Self::escape_like_pattern(prefix));
        }
        format!("%{}%", Self::escape_like_pattern(term))
    }

    fn is_prefix_wildcard_term(term: &str) -> bool {
        matches!(term.strip_suffix('*'), Some(prefix) if !prefix.is_empty())
    }

    fn escape_like_pattern(term: &str) -> String {
        let mut escaped = String::with_capacity(term.len());
        for ch in term.chars() {
            if matches!(ch, '%' | '_' | '\\') {
                escaped.push('\\');
            }
            escaped.push(ch);
        }
        escaped
    }

    fn like_fallback_matches(text: &str, term: &str) -> bool {
        let text = text.to_lowercase();
        if let Some(prefix) = term.strip_suffix('*')
            && !prefix.is_empty()
        {
            let prefix = prefix.to_lowercase();
            return text
                .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
                .any(|token| token.starts_with(&prefix));
        }
        text.contains(&term.to_lowercase())
    }

    /// Vector similarity search: scan embeddings and compute cosine similarity.
    ///
    /// Optional `category` and `session_id` filters reduce full-table scans
    /// when the caller already knows the scope of relevant memories.
    pub fn vector_search(
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

    /// List memories by time range (used when query is empty).
    async fn recall_by_time_only(
        &self,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let conn = self.conn.clone();
        let sid = session_id.map(String::from);
        let since_owned = since.map(String::from);
        let until_owned = until.map(String::from);

        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let conn = conn.lock();
            let since_ref = since_owned.as_deref();
            let until_ref = until_owned.as_deref();

            let mut sql =
                "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, a.alias, m.agent_id \
                 FROM memories m LEFT JOIN agents a ON a.id = m.agent_id \
                 WHERE m.superseded_by IS NULL AND 1=1"
                    .to_string();
            let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            let mut idx = 1;

            if let Some(sid) = sid.as_deref() {
                let _ = write!(sql, " AND m.session_id = ?{idx}");
                param_values.push(Box::new(sid.to_string()));
                idx += 1;
            }
            if let Some(s) = since_ref {
                let _ = write!(sql, " AND m.created_at >= ?{idx}");
                param_values.push(Box::new(s.to_string()));
                idx += 1;
            }
            if let Some(u) = until_ref {
                let _ = write!(sql, " AND m.created_at <= ?{idx}");
                param_values.push(Box::new(u.to_string()));
                idx += 1;
            }
            let _ = write!(sql, " ORDER BY m.updated_at DESC LIMIT ?{idx}");
            #[allow(clippy::cast_possible_wrap)]
            param_values.push(Box::new(limit as i64));

            let mut stmt = conn.prepare(&sql)?;
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
                    score: None,
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    agent_alias: row.get(9)?,
                    agent_id: row.get(10)?,
                })
            })?;

            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await?
    }
}

#[async_trait]
impl Memory for SqliteMemory {
    fn name(&self) -> &str {
        "sqlite"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        // Trait-level `store` has no agent context; route through
        // `store_with_agent` so the row gets attributed to the default
        // agent (the NOT NULL FK on `agent_id` rejects unattributed
        // inserts).
        self.store_with_agent(key, content, category, session_id, None, None, None)
            .await
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // Time-only query: list by time range when no keywords.
        // Treat only a bare "*" as the same recent-entry request; keep
        // real wildcard searches such as "wild*" on the keyword path.
        if is_recent_recall_query(query) {
            return self
                .recall_by_time_only(limit, session_id, since, until)
                .await;
        }

        // Compute query embedding only when needed (skip for BM25-only mode)
        let query_embedding = if self.search_mode == SearchMode::Bm25 {
            None
        } else {
            self.get_or_compute_embedding(query).await?
        };

        let conn = self.conn.clone();
        let query = query.to_string();
        let sid = session_id.map(String::from);
        let since_owned = since.map(String::from);
        let until_owned = until.map(String::from);
        let vector_weight = self.vector_weight;
        let keyword_weight = self.keyword_weight;
        let search_mode = self.search_mode.clone();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let conn = conn.lock();
            let session_ref = sid.as_deref();
            let since_ref = since_owned.as_deref();
            let until_ref = until_owned.as_deref();

            // FTS5 BM25 keyword search (skip for embedding-only mode)
            let keyword_results = if search_mode == SearchMode::Embedding {
                Vec::new()
            } else {
                Self::fts5_search(&conn, &query, limit * 2).unwrap_or_default()
            };

            // Vector similarity search (skip for BM25-only mode)
            let vector_results = if search_mode == SearchMode::Bm25 {
                Vec::new()
            } else if let Some(ref qe) = query_embedding {
                Self::vector_search(&conn, qe, limit * 2, None, session_ref).unwrap_or_default()
            } else {
                Vec::new()
            };

            // Merge results based on search mode
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
            } else if keyword_results.is_empty() {
                vector_results
                    .iter()
                    .map(|(id, score)| vector::ScoredResult {
                        id: id.clone(),
                        vector_score: Some(*score),
                        keyword_score: None,
                        final_score: *score,
                    })
                    .collect::<Vec<_>>()
            } else {
                vector::hybrid_merge(
                    &vector_results,
                    &keyword_results,
                    vector_weight,
                    keyword_weight,
                    limit,
                )
            };

            // Fetch full entries for merged results in a single query
            // instead of N round-trips (N+1 pattern).
            let mut results = Vec::new();
            if !merged.is_empty() {
                let placeholders: String = (1..=merged.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, a.alias, m.agent_id \
                     FROM memories m LEFT JOIN agents a ON a.id = m.agent_id \
                     WHERE m.superseded_by IS NULL AND m.id IN ({placeholders})"
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
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<f64>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                    ))
                })?;

                let mut entry_map = std::collections::HashMap::new();
                for row in rows {
                    let (id, key, content, cat, ts, sid, ns, imp, sup, alias, aid) = row?;
                    entry_map.insert(id, (key, content, cat, ts, sid, ns, imp, sup, alias, aid));
                }

                for scored in &merged {
                    if let Some((key, content, cat, ts, sid, ns, imp, sup, alias, aid)) = entry_map.remove(&scored.id) {
                        if let Some(s) = since_ref
                            && ts.as_str() < s {
                                continue;
                            }
                        if let Some(u) = until_ref
                            && ts.as_str() > u {
                                continue;
                            }
                        let entry = MemoryEntry {
                            id: scored.id.clone(),
                            key,
                            content,
                            category: Self::str_to_category(&cat),
                            timestamp: ts,
                            session_id: sid,
                            score: Some(f64::from(scored.final_score)),
                            namespace: ns.unwrap_or_else(|| "default".into()),
                            importance: imp,
                            superseded_by: sup,
                            agent_alias: alias,
                            agent_id: aid,
                        };
                        if let Some(filter_sid) = session_ref
                            && entry.session_id.as_deref() != Some(filter_sid) {
                                continue;
                            }
                        results.push(entry);
                    }
                }
            }

            // If hybrid returned nothing, fall back to LIKE search.
            if results.is_empty() {
                const MAX_LIKE_KEYWORDS: usize = 8;
                let raw_keywords: Vec<String> = query
                    .split_whitespace()
                    .take(MAX_LIKE_KEYWORDS)
                    .map(str::to_string)
                    .collect();
                if !raw_keywords.is_empty() {
                    let needs_prefix_filter = raw_keywords
                        .iter()
                        .any(|keyword| Self::is_prefix_wildcard_term(keyword));
                    let sql_limit = if needs_prefix_filter {
                        limit.saturating_mul(8).min(limit.saturating_add(512))
                    } else {
                        limit
                    };
                    let patterns: Vec<String> = raw_keywords
                        .iter()
                        .map(|keyword| Self::like_search_pattern(keyword))
                        .collect();
                    let conditions: Vec<String> = patterns
                        .iter()
                        .enumerate()
                        .map(|(i, _)| {
                            format!(
                                "(m.content LIKE ?{} ESCAPE '\\' OR m.key LIKE ?{} ESCAPE '\\')",
                                i * 2 + 1,
                                i * 2 + 2
                            )
                        })
                        .collect();
                    let where_clause = conditions.join(" OR ");
                    let mut param_idx = patterns.len() * 2 + 1;
                    let mut time_conditions = String::new();
                    if since_ref.is_some() {
                        let _ = write!(time_conditions, " AND m.created_at >= ?{param_idx}");
                        param_idx += 1;
                    }
                    if until_ref.is_some() {
                        let _ = write!(time_conditions, " AND m.created_at <= ?{param_idx}");
                        param_idx += 1;
                    }
                    let sql = format!(
                        "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, a.alias, m.agent_id
                         FROM memories m LEFT JOIN agents a ON a.id = m.agent_id
                         WHERE m.superseded_by IS NULL AND ({where_clause}){time_conditions}
                         ORDER BY m.updated_at DESC
                         LIMIT ?{param_idx}"
                    );
                    let mut stmt = conn.prepare(&sql)?;
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    for kw in &patterns {
                        param_values.push(Box::new(kw.clone()));
                        param_values.push(Box::new(kw.clone()));
                    }
                    if let Some(s) = since_ref {
                        param_values.push(Box::new(s.to_string()));
                    }
                    if let Some(u) = until_ref {
                        param_values.push(Box::new(u.to_string()));
                    }
                    #[allow(clippy::cast_possible_wrap)]
                    param_values.push(Box::new(sql_limit as i64));
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
                            namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                            importance: row.get(7)?,
                            superseded_by: row.get(8)?,
                            agent_alias: row.get(9)?,
                            agent_id: row.get(10)?,
                        })
                    })?;
                    for row in rows {
                        let entry = row?;
                        if let Some(sid) = session_ref
                            && entry.session_id.as_deref() != Some(sid) {
                                continue;
                            }
                        if needs_prefix_filter
                            && !raw_keywords.iter().any(|keyword| {
                                Self::like_fallback_matches(&entry.key, keyword)
                                    || Self::like_fallback_matches(&entry.content, keyword)
                            })
                        {
                            continue;
                        }
                        results.push(entry);
                        if results.len() >= limit {
                            break;
                        }
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
            let mut stmt = conn.prepare(
                "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, a.alias, m.agent_id \
                 FROM memories m LEFT JOIN agents a ON a.id = m.agent_id \
                 WHERE m.key = ?1",
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
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    agent_alias: row.get(9)?,
                    agent_id: row.get(10)?,
                })
            })?;

            match rows.next() {
                Some(Ok(entry)) => Ok(Some(entry)),
                _ => Ok(None),
            }
        })
        .await?
    }

    async fn get_for_agent(
        &self,
        key: &str,
        agent_id: &str,
    ) -> anyhow::Result<Option<MemoryEntry>> {
        let conn = self.conn.clone();
        let key = key.to_string();
        let agent_id = agent_id.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<MemoryEntry>> {
            let conn = conn.lock();
            let mut stmt = conn.prepare(
                "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, a.alias, m.agent_id \
                 FROM memories m LEFT JOIN agents a ON a.id = m.agent_id \
                 WHERE m.key = ?1 AND m.agent_id = ?2",
            )?;

            let mut rows = stmt.query_map(params![key, agent_id], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    agent_alias: row.get(9)?,
                    agent_id: row.get(10)?,
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
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    agent_alias: row.get(9)?,
                    agent_id: row.get(10)?,
                })
            };

            if let Some(ref cat) = category {
                let cat_str = Self::category_to_str(cat);
                let mut stmt = conn.prepare(
                    "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, a.alias, m.agent_id
                     FROM memories m LEFT JOIN agents a ON a.id = m.agent_id
                     WHERE m.superseded_by IS NULL AND m.category = ?1 ORDER BY m.updated_at DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![cat_str, DEFAULT_LIST_LIMIT], row_mapper)?;
                for row in rows {
                    let entry = row?;
                    if let Some(sid) = session_ref
                        && entry.session_id.as_deref() != Some(sid) {
                            continue;
                        }
                    results.push(entry);
                }
            } else {
                let mut stmt = conn.prepare(
                    "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, a.alias, m.agent_id
                     FROM memories m LEFT JOIN agents a ON a.id = m.agent_id
                     WHERE m.superseded_by IS NULL ORDER BY m.updated_at DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![DEFAULT_LIST_LIMIT], row_mapper)?;
                for row in rows {
                    let entry = row?;
                    if let Some(sid) = session_ref
                        && entry.session_id.as_deref() != Some(sid) {
                            continue;
                        }
                    results.push(entry);
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

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.clone();
        let key = key.to_string();
        let agent_id = agent_id.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let conn = conn.lock();
            let affected = conn.execute(
                "DELETE FROM memories WHERE key = ?1 AND agent_id = ?2",
                params![key, agent_id],
            )?;
            Ok(affected > 0)
        })
        .await?
    }

    async fn purge_namespace(&self, namespace: &str) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let namespace = namespace.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = conn.lock();
            let affected = conn.execute(
                "DELETE FROM memories WHERE namespace = ?1",
                params![namespace],
            )?;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(affected)
        })
        .await?
    }

    async fn purge_session(&self, session_id: &str) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let session_id = session_id.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = conn.lock();
            let affected = conn.execute(
                "DELETE FROM memories WHERE session_id = ?1",
                params![session_id],
            )?;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(affected)
        })
        .await?
    }

    async fn purge_session_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
    ) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = conn.lock();
            let affected = conn.execute(
                "DELETE FROM memories WHERE session_id = ?1 AND agent_id = ?2",
                params![session_id, agent_id],
            )?;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(affected)
        })
        .await?
    }

    async fn purge_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let agent_alias = agent_alias.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = conn.lock();
            // `agent_alias` is the human alias, but `memories.agent_id` holds
            // the agent's UUID (FK → agents.id). Resolve alias → id via the same
            // subselect the insert path uses (`store_with_agent`); binding the
            // alias straight into agent_id matches zero rows and silently
            // no-ops. An unknown alias yields a NULL subselect → matches
            // nothing, which is the correct outcome.
            let affected = conn.execute(
                "DELETE FROM memories WHERE agent_id = (SELECT id FROM agents WHERE alias = ?1 LIMIT 1)",
                params![agent_alias],
            )?;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(affected)
        })
        .await?
    }

    async fn rename_agent(&self, from: &str, to: &str) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let from = from.to_string();
        let to = to.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = conn.lock();
            // Memory rows ride `memories.agent_id` (FK → agents.id, a stable
            // UUID); only the human `alias` column moves, so this is a single
            // agents-row update. An unknown `from` matches nothing → Ok(0).
            //
            // Collision-safety: `agents.alias` is UNIQUE, and deleting an agent
            // purges its memories but leaves the `agents` row behind (an orphan
            // holding the alias). A bare UPDATE onto a previously-used-then-
            // deleted `to` alias would hit the UNIQUE constraint and fail. We
            // hold the connection lock across the whole sequence (single writer),
            // so: refuse if `to` still has memory rows (a genuine conflict we
            // won't silently merge), otherwise drop the orphan `to` row and
            // proceed. (`COUNT(*)` over a NULL subselect when no `to` row exists
            // is 0, so the common no-collision path falls straight through.)
            let to_rows: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE agent_id = (SELECT id FROM agents WHERE alias = ?1 LIMIT 1)",
                params![to],
                |row| row.get(0),
            )?;
            if to_rows > 0 {
                anyhow::bail!(
                    "cannot rename agent memory to `{to}`: an existing memory store under that alias has {to_rows} row(s); refusing to merge"
                );
            }
            // Drop any orphan `to` agents row (verified above to own no memories).
            conn.execute("DELETE FROM agents WHERE alias = ?1", params![to])?;
            let affected = conn.execute(
                "UPDATE agents SET alias = ?2 WHERE alias = ?1",
                params![from, to],
            )?;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(affected)
        })
        .await?
    }

    async fn count_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let agent_alias = agent_alias.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let conn = conn.lock();
            // Mirror `rename_agent`: it moves the `agents` row (alias -> id), not
            // the memory rows, so residue is the presence of that alias row (0 or
            // 1). A memory-row count would miss an agent with an `agents` row but
            // no memories - a real lag `rename_agent` would still re-point.
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM agents WHERE alias = ?1",
                params![agent_alias],
                |row| row.get(0),
            )?;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(count as usize)
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

    /// Rebuild backend indexes: FTS tables and missing embedding vectors.
    ///
    /// Step 1 rebuilds the FTS5 index unconditionally (idempotent, cheap).
    /// Step 2 fills in vectors for every row with `embedding IS NULL` using
    /// the configured embedder. If interrupted, re-running is safe — only
    /// rows still missing a vector are re-processed. Intended to be run
    /// after bulk writes that didn't go through `store()` (e.g. `zeroclaw
    /// migrate openclaw`, which uses `NoopEmbedding` for speed). Returns
    /// the number of rows that received a new embedding; returns 0 if the
    /// embedder has no dimensions (Noop) or if everything is already
    /// embedded.
    async fn reindex(&self) -> anyhow::Result<usize> {
        // Step 1: Rebuild FTS5 (always safe, cheap)
        {
            let conn = self.conn.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let conn = conn.lock();
                conn.execute_batch("INSERT INTO memories_fts(memories_fts) VALUES('rebuild');")?;
                Ok(())
            })
            .await??;
        }

        // Step 2: Re-embed memories with NULL vectors, if embedder is configured
        if self.embedder.dimensions() == 0 {
            return Ok(0);
        }

        let conn = self.conn.clone();
        let entries: Vec<(String, String)> = tokio::task::spawn_blocking(move || {
            let conn = conn.lock();
            let mut stmt =
                conn.prepare("SELECT id, content FROM memories WHERE embedding IS NULL")?;
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

    async fn export(&self, filter: &ExportFilter) -> anyhow::Result<Vec<MemoryEntry>> {
        let conn = self.conn.clone();
        let filter = filter.clone();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let conn = conn.lock();
            let mut sql =
                "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, a.alias, m.agent_id \
                 FROM memories m LEFT JOIN agents a ON a.id = m.agent_id \
                 WHERE 1=1"
                    .to_string();
            let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            let mut idx = 1;

            if let Some(ref ns) = filter.namespace {
                let _ = write!(sql, " AND m.namespace = ?{idx}");
                param_values.push(Box::new(ns.clone()));
                idx += 1;
            }
            if let Some(ref sid) = filter.session_id {
                let _ = write!(sql, " AND m.session_id = ?{idx}");
                param_values.push(Box::new(sid.clone()));
                idx += 1;
            }
            if let Some(ref cat) = filter.category {
                let _ = write!(sql, " AND m.category = ?{idx}");
                param_values.push(Box::new(Self::category_to_str(cat)));
                idx += 1;
            }
            if let Some(ref since) = filter.since {
                let _ = write!(sql, " AND m.created_at >= ?{idx}");
                param_values.push(Box::new(since.clone()));
                idx += 1;
            }
            if let Some(ref until) = filter.until {
                let _ = write!(sql, " AND m.created_at <= ?{idx}");
                param_values.push(Box::new(until.clone()));
                let _ = idx;
            }
            sql.push_str(" ORDER BY m.created_at ASC");

            let mut stmt = conn.prepare(&sql)?;
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
                    score: None,
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    agent_alias: row.get(9)?,
                    agent_id: row.get(10)?,
                })
            })?;

            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await?
    }

    async fn export_agent(&self, agent_alias: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        let conn = self.conn.clone();
        let agent_alias = agent_alias.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let conn = conn.lock();
            let mut stmt = conn.prepare(
                "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, a.alias, m.agent_id \
                 FROM memories m LEFT JOIN agents a ON a.id = m.agent_id \
                 WHERE m.agent_id = (SELECT id FROM agents WHERE alias = ?1 LIMIT 1) \
                 ORDER BY m.created_at ASC",
            )?;
            let rows = stmt.query_map(params![agent_alias], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    agent_alias: row.get(9)?,
                    agent_id: row.get(10)?,
                })
            })?;
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        })
        .await?
    }

    async fn recall_namespaced(
        &self,
        namespace: &str,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let entries = self
            .recall(query, limit * 2, session_id, since, until)
            .await?;
        let filtered: Vec<MemoryEntry> = entries
            .into_iter()
            .filter(|e| e.namespace == namespace)
            .take(limit)
            .collect();
        Ok(filtered)
    }

    async fn store_with_metadata(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
    ) -> anyhow::Result<()> {
        // Same routing rule as `store`: no agent context at the trait
        // boundary, so attribute to the default agent through
        // `store_with_agent`.
        self.store_with_agent(
            key, content, category, session_id, namespace, importance, None,
        )
        .await
    }

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        // Graceful degrade: an embedding failure (provider 404/401, rate limit,
        // outage, rotated key) must NOT discard the write. Persist the row with
        // a NULL vector and log a recoverable warning — `zeroclaw memory
        // reindex` backfills NULL embeddings from the retained `content` once
        // the embedder is healthy again. Propagating the error here previously
        // turned a transient credential fault into silent, permanent data loss.
        let embedding_bytes = match self.get_or_compute_embedding(content).await {
            Ok(emb) => emb.map(|emb| vector::vec_to_bytes(&emb)),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "key": key,
                            "error": format!("{e}"),
                        })),
                    "memory store: embedding failed; persisting row without a vector \
                     (run `zeroclaw memory reindex` to backfill once the embedder recovers)"
                );
                None
            }
        };

        let conn = self.conn.clone();
        let key = key.to_string();
        let content = content.to_string();
        let sid = session_id.map(String::from);
        let ns = namespace.unwrap_or("default").to_string();
        let imp = importance.unwrap_or(0.5);
        let aid = agent_id.map(String::from);

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = conn.lock();
            let now = Local::now().to_rfc3339();
            let cat = Self::category_to_str(&category);
            let id = Uuid::new_v4().to_string();

            // Uniqueness is per (agent_id, key): two agents may hold rows
            // with the same key without clobbering each other. `agent_id`
            // falls back to the synthesized default agent when the caller
            // didn't supply one (callers going through AgentScopedMemory
            // always do).
            conn.execute(
                "INSERT INTO memories (id, key, content, category, embedding, created_at, updated_at, session_id, namespace, importance, agent_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, COALESCE(?11, (SELECT id FROM agents WHERE alias = 'default' LIMIT 1)))
                 ON CONFLICT(agent_id, key) DO UPDATE SET
                    content = excluded.content,
                    category = excluded.category,
                    embedding = excluded.embedding,
                    updated_at = excluded.updated_at,
                    session_id = excluded.session_id,
                    namespace = excluded.namespace,
                    importance = excluded.importance",
                params![id, key, content, cat, embedding_bytes, now, now, sid, ns, imp, aid],
            )?;
            Ok(())
        })
        .await?
    }

    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // Empty allowlist means "no agent filter": fall back to plain
        // recall. The wrapper always includes the bound agent's UUID,
        // so a non-empty allowlist is the live-runtime case.
        if allowed_agent_ids.is_empty() {
            return self.recall(query, limit, session_id, since, until).await;
        }

        let full_candidate_limit = self.count().await?.max(limit);
        let raw = self
            .recall(query, full_candidate_limit, session_id, since, until)
            .await?;
        if raw.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.conn.clone();
        let ids: Vec<String> = raw.iter().map(|e| e.id.clone()).collect();
        let allowed: Vec<String> = allowed_agent_ids.iter().map(|s| (*s).to_string()).collect();

        // Single SQL pass that returns only the candidate IDs whose
        // agent_id is on the allowlist. Legacy NULL-agent_id rows do
        // not match (the V3 migration backfills `default`, and the
        // NOT NULL FK rejects new NULLs), so cross-agent leakage of
        // unattributed rows that an earlier post-fetch fall-through
        // would have allowed is closed at the query boundary.
        let kept: HashSet<String> =
            tokio::task::spawn_blocking(move || -> anyhow::Result<HashSet<String>> {
                let conn = conn.lock();
                let id_placeholders: String = (1..=ids.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let agent_placeholders: String = (ids.len() + 1..=ids.len() + allowed.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "SELECT id FROM memories \
                     WHERE id IN ({id_placeholders}) \
                       AND agent_id IN ({agent_placeholders})"
                );
                let mut stmt = conn.prepare(&sql)?;
                let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
                    Vec::with_capacity(ids.len() + allowed.len());
                for id in &ids {
                    params.push(Box::new(id.clone()) as Box<dyn rusqlite::types::ToSql>);
                }
                for aid in &allowed {
                    params.push(Box::new(aid.clone()) as Box<dyn rusqlite::types::ToSql>);
                }
                let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                    params.iter().map(AsRef::as_ref).collect();
                let rows = stmt.query_map(params_ref.as_slice(), |row| row.get::<_, String>(0))?;
                let mut set = HashSet::new();
                for row in rows {
                    set.insert(row?);
                }
                Ok(set)
            })
            .await??;

        Ok(raw
            .into_iter()
            .filter(|e| kept.contains(&e.id))
            .take(limit)
            .collect())
    }

    async fn ensure_agent_uuid(&self, alias: &str) -> anyhow::Result<String> {
        let conn = self.conn.clone();
        let alias = alias.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let conn = conn.lock();
            zeroclaw_config::schema::v2::sqlite_ensure_agent_uuid(&conn, &alias)
        })
        .await?
    }
}

impl ::zeroclaw_api::attribution::Attributable for SqliteMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::Sqlite)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_sqlite() -> (TempDir, SqliteMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
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

        let results = mem.recall("Rust", 10, None, None, None).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|r| r.content.to_lowercase().contains("rust"))
        );
    }

    #[tokio::test]
    async fn sqlite_recall_for_agents_does_not_lose_allowed_rows_behind_disallowed_matches() {
        let (_tmp, mem) = temp_sqlite();
        let alpha = mem.ensure_agent_uuid("alpha").await.unwrap();
        let rogue = mem.ensure_agent_uuid("rogue").await.unwrap();

        for idx in 0..12 {
            mem.store_with_agent(
                &format!("rogue-{idx}"),
                "needle disallowed row",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(&rogue),
            )
            .await
            .unwrap();
        }
        mem.store_with_agent(
            "alpha-allowed",
            "needle allowed row",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some(&alpha),
        )
        .await
        .unwrap();

        let results = mem
            .recall_for_agents(&[alpha.as_str()], "needle", 1, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "alpha-allowed");
    }

    #[tokio::test]
    async fn sqlite_purge_agent_deletes_only_that_agents_rows() {
        let (_tmp, mem) = temp_sqlite();
        let alpha = mem.ensure_agent_uuid("alpha").await.unwrap();
        let rogue = mem.ensure_agent_uuid("rogue").await.unwrap();

        for idx in 0..3 {
            mem.store_with_agent(
                &format!("alpha-{idx}"),
                "alpha row",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(&alpha),
            )
            .await
            .unwrap();
        }
        for idx in 0..2 {
            mem.store_with_agent(
                &format!("rogue-{idx}"),
                "rogue row",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(&rogue),
            )
            .await
            .unwrap();
        }
        assert_eq!(mem.count().await.unwrap(), 5);

        // Purge by ALIAS (not UUID). The regression: purge_agent bound the
        // alias straight into the agent_id column, matched zero rows, and
        // returned Ok(0) — so deleting an agent silently kept its memories.
        // The fix resolves alias → id and must delete exactly alpha's rows.
        let purged = mem.purge_agent("alpha").await.unwrap();
        assert_eq!(purged, 3, "purge_agent must delete exactly alpha's rows");
        assert_eq!(mem.count().await.unwrap(), 2, "rogue's rows must survive");

        // Unknown alias → NULL id subselect → deletes nothing, returns 0.
        let purged_ghost = mem.purge_agent("ghost").await.unwrap();
        assert_eq!(purged_ghost, 0);
        assert_eq!(mem.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn sqlite_rename_agent_repoints_rows_under_new_alias() {
        let (_tmp, mem) = temp_sqlite();
        let alpha = mem.ensure_agent_uuid("alpha").await.unwrap();
        for idx in 0..3 {
            mem.store_with_agent(
                &format!("alpha-{idx}"),
                "alpha row",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(&alpha),
            )
            .await
            .unwrap();
        }

        // Rename alpha → beta: memory rows ride the UUID, so this updates exactly
        // one `agents` row and the rows now resolve under the new alias.
        let renamed = mem.rename_agent("alpha", "beta").await.unwrap();
        assert_eq!(renamed, 1, "exactly one agents row re-aliased");

        // The rows now resolve under `beta`, and `alpha` resolves to nothing.
        assert_eq!(mem.export_agent("beta").await.unwrap().len(), 3);
        assert_eq!(mem.export_agent("alpha").await.unwrap().len(), 0);
        assert_eq!(mem.count().await.unwrap(), 3, "no rows lost on rename");

        // Unknown source → nothing updated.
        assert_eq!(mem.rename_agent("ghost", "phantom").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn sqlite_rename_agent_reclaims_orphan_and_refuses_live_collision() {
        let (_tmp, mem) = temp_sqlite();
        let alpha = mem.ensure_agent_uuid("alpha").await.unwrap();
        mem.store_with_agent(
            "a-0",
            "alpha row",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some(&alpha),
        )
        .await
        .unwrap();

        // Simulate a prior delete of `beta`: its memories were purged but the
        // agents row survives (delete never removes it) — an orphan in the
        // UNIQUE alias slot. A bare UPDATE alpha→beta would hit the constraint.
        let _beta = mem.ensure_agent_uuid("beta").await.unwrap();
        assert_eq!(mem.purge_agent("beta").await.unwrap(), 0); // no memories anyway
        // Rename succeeds: the orphan `beta` row is dropped, alpha→beta proceeds.
        assert_eq!(mem.rename_agent("alpha", "beta").await.unwrap(), 1);
        assert_eq!(mem.export_agent("beta").await.unwrap().len(), 1);
        assert_eq!(mem.export_agent("alpha").await.unwrap().len(), 0);

        // Now `beta` has a live memory. Renaming another agent ONTO it must
        // refuse (we won't silently merge two agents' memories).
        let gamma = mem.ensure_agent_uuid("gamma").await.unwrap();
        mem.store_with_agent(
            "g-0",
            "gamma row",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some(&gamma),
        )
        .await
        .unwrap();
        let err = mem.rename_agent("gamma", "beta").await.unwrap_err();
        assert!(
            err.to_string().contains("refusing to merge"),
            "expected merge-refusal, got: {err}"
        );
        // Nothing changed: both still resolve under their own aliases.
        assert_eq!(mem.export_agent("beta").await.unwrap().len(), 1);
        assert_eq!(mem.export_agent("gamma").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sqlite_export_agent_returns_only_that_agents_rows() {
        let (_tmp, mem) = temp_sqlite();
        let alpha = mem.ensure_agent_uuid("alpha").await.unwrap();
        let rogue = mem.ensure_agent_uuid("rogue").await.unwrap();
        for idx in 0..3 {
            mem.store_with_agent(
                &format!("alpha-{idx}"),
                "alpha row",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(&alpha),
            )
            .await
            .unwrap();
        }
        mem.store_with_agent(
            "rogue-0",
            "rogue row",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some(&rogue),
        )
        .await
        .unwrap();

        let exported = mem.export_agent("alpha").await.unwrap();
        assert_eq!(exported.len(), 3, "export only alpha's rows");
        assert!(exported.iter().all(|e| e.key.starts_with("alpha-")));
        assert_eq!(mem.export_agent("rogue").await.unwrap().len(), 1);
        assert!(mem.export_agent("ghost").await.unwrap().is_empty());
        // export does NOT delete.
        assert_eq!(mem.count().await.unwrap(), 4);
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

        let results = mem.recall("fast safe", 10, None, None, None).await.unwrap();
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
        let results = mem
            .recall("javascript", 10, None, None, None)
            .await
            .unwrap();
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
            let mem = SqliteMemory::new("test", tmp.path()).unwrap();
            mem.store("persist", "I survive restarts", MemoryCategory::Core, None)
                .await
                .unwrap();
        }

        // Reopen
        let mem2 = SqliteMemory::new("test", tmp.path()).unwrap();
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

        let results = mem.recall("Rust", 10, None, None, None).await.unwrap();
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

        let results = mem.recall("quick dog", 10, None, None, None).await.unwrap();
        assert!(!results.is_empty());
        // "The quick dog runs fast" matches both terms
        assert!(results[0].content.contains("quick"));
    }

    #[tokio::test]
    async fn recall_empty_query_returns_recent_entries() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "data", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Empty query = time-only mode: returns recent entries
        let results = mem.recall("", 10, None, None, None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "a");
    }

    #[tokio::test]
    async fn recall_whitespace_query_returns_recent_entries() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "data", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Whitespace-only query = time-only mode: returns recent entries
        let results = mem.recall("   ", 10, None, None, None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "a");
    }

    #[tokio::test]
    async fn recall_star_query_returns_recent_entries() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "first memory", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "second memory", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("*", 10, None, None, None).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|entry| entry.key == "a"));
        assert!(results.iter().any(|entry| entry.key == "b"));
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
        let mem = SqliteMemory::with_embedder(
            "test",
            tmp.path(),
            embedder,
            0.7,
            0.3,
            1000,
            Some(5),
            SearchMode::default(),
        );
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
            "test",
            tmp.path(),
            Arc::new(super::super::embeddings::NoopEmbedding),
            0.7,
            0.3,
            1000,
            Some(2),
            SearchMode::default(),
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

    // ── Graceful degrade on embedding failure ────────────────────

    /// Embedder that advertises a real dimension but always fails to embed,
    /// simulating a provider 404/401/outage (e.g. a wrong or revoked embedding
    /// key — exactly the live failure that silently dropped 6 days of writes).
    struct FailingEmbedding;

    #[async_trait::async_trait]
    impl super::super::embeddings::EmbeddingProvider for FailingEmbedding {
        fn name(&self) -> &str {
            "failing"
        }
        fn dimensions(&self) -> usize {
            1536
        }
        async fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("Embedding API error 404 Not Found — \"Requested entity was not found.\"")
        }
    }

    #[tokio::test]
    async fn store_degrades_gracefully_when_embedding_fails() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::with_embedder(
            "test",
            tmp.path(),
            Arc::new(FailingEmbedding),
            0.7,
            0.3,
            1000,
            None,
            SearchMode::default(),
        )
        .unwrap();

        // A failing embedder must NOT cost us the write. The row has to persist
        // with a NULL vector rather than the whole store aborting — that abort
        // was the data-loss bug. `reindex` can backfill the vector later.
        mem.store(
            "survives",
            "this content must be retained",
            MemoryCategory::Core,
            None,
        )
        .await
        .expect("store must succeed even when the embedder fails");

        assert_eq!(mem.count().await.unwrap(), 1, "row must be persisted");
        let entry = mem.get("survives").await.unwrap().unwrap();
        assert_eq!(entry.content, "this content must be retained");
    }

    // ── With-embedder constructor test ───────────────────────────

    #[test]
    fn with_embedder_noop() {
        let tmp = TempDir::new().unwrap();
        let embedder = Arc::new(super::super::embeddings::NoopEmbedding);
        let mem = SqliteMemory::with_embedder(
            "test",
            tmp.path(),
            embedder,
            0.7,
            0.3,
            1000,
            None,
            SearchMode::default(),
        );
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
        let results = mem.recall("reindex", 10, None, None, None).await.unwrap();
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

        let results = mem
            .recall("common keyword", 5, None, None, None)
            .await
            .unwrap();
        assert!(results.len() <= 5);
    }

    // ── Score presence test ──────────────────────────────────────

    #[tokio::test]
    async fn recall_results_have_scores() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("s1", "scored result test", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("scored", 10, None, None, None).await.unwrap();
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
        let results = mem.recall("\"hello\"", 10, None, None, None).await.unwrap();
        // May or may not match depending on FTS5 escaping, but must not error
        assert!(results.len() <= 10);
    }

    #[tokio::test]
    async fn recall_with_asterisk_in_query() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a1", "wildcard test content", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b1", "unrelated recent content", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("wild*", 10, None, None, None).await.unwrap();
        assert!(results.iter().any(|entry| entry.key == "a1"));
        assert!(results.iter().all(|entry| entry.key != "b1"));
    }

    #[tokio::test]
    async fn recall_prefix_wildcard_like_fallback_keeps_token_prefix() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::with_embedder(
            "test",
            tmp.path(),
            Arc::new(super::super::embeddings::NoopEmbedding),
            0.7,
            0.3,
            1000,
            None,
            SearchMode::Embedding,
        )
        .unwrap();
        mem.store("a1", "fallback wildcard token", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b1", "fallback unwild token", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("wild*", 10, None, None, None).await.unwrap();
        assert!(results.iter().any(|entry| entry.key == "a1"));
        assert!(results.iter().all(|entry| entry.key != "b1"));
    }

    #[tokio::test]
    async fn recall_prefix_wildcard_like_fallback_overfetches_filtered_rows() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::with_embedder(
            "test",
            tmp.path(),
            Arc::new(super::super::embeddings::NoopEmbedding),
            0.7,
            0.3,
            1000,
            None,
            SearchMode::Embedding,
        )
        .unwrap();
        mem.store(
            "real",
            "fallback wildcard token",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        for i in 0..3 {
            mem.store(
                &format!("noise{i}"),
                "fallback unwild token",
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
        }
        {
            let conn = mem.conn.lock();
            conn.execute(
                "UPDATE memories SET updated_at = ?1 WHERE key = ?2",
                rusqlite::params!["2026-05-03T00:00:00Z", "real"],
            )
            .unwrap();
            for i in 0..3 {
                conn.execute(
                    "UPDATE memories SET updated_at = ?1 WHERE key = ?2",
                    rusqlite::params![format!("2026-05-03T00:00:0{}Z", i + 1), format!("noise{i}")],
                )
                .unwrap();
            }
        }

        let results = mem.recall("wild*", 1, None, None, None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "real");
    }

    #[tokio::test]
    async fn recall_with_parentheses_in_query() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("p1", "function call test", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem
            .recall("function()", 10, None, None, None)
            .await
            .unwrap();
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
            .recall("'; DROP TABLE memories; --", 10, None, None, None)
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
        let results = mem.recall("x", 10, None, None, None).await.unwrap();
        // Should not crash; may or may not find results
        assert!(results.len() <= 10);
    }

    #[tokio::test]
    async fn recall_limit_zero() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "some content", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("some", 0, None, None, None).await.unwrap();
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
        let results = mem
            .recall("matching content", 1, None, None, None)
            .await
            .unwrap();
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
        let results = mem.recall("rust", 10, None, None, None).await.unwrap();
        assert!(!results.is_empty(), "Should match by key");
    }

    #[tokio::test]
    async fn recall_unicode_query() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("jp", "日本語のテスト", MemoryCategory::Core, None)
            .await
            .unwrap();
        let results = mem.recall("日本語", 10, None, None, None).await.unwrap();
        assert!(!results.is_empty());
    }

    // ── Edge cases: schema idempotency ───────────────────────────

    #[tokio::test]
    async fn schema_idempotent_reopen() {
        let tmp = TempDir::new().unwrap();
        {
            let mem = SqliteMemory::new("test", tmp.path()).unwrap();
            mem.store("k1", "v1", MemoryCategory::Core, None)
                .await
                .unwrap();
        }
        // Open again — init_schema runs again on existing DB
        let mem2 = SqliteMemory::new("test", tmp.path()).unwrap();
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
        let _m1 = SqliteMemory::new("test", tmp.path()).unwrap();
        let _m2 = SqliteMemory::new("test", tmp.path()).unwrap();
        let m3 = SqliteMemory::new("test", tmp.path()).unwrap();
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
        let results = mem
            .recall("phantom memory", 10, None, None, None)
            .await
            .unwrap();
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
        let results = mem.recall("reindex", 10, None, None, None).await.unwrap();
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

    // ── Bulk deletion tests ───────────────────────────────────────

    #[tokio::test]
    async fn sqlite_purge_namespace_deletes_only_all_matching_entries() {
        let (_tmp, mem) = temp_sqlite();

        mem.store_with_metadata("a", "data", MemoryCategory::Core, None, Some("ns1"), None)
            .await
            .unwrap();
        mem.store_with_metadata("b", "data", MemoryCategory::Core, None, Some("ns2"), None)
            .await
            .unwrap();

        let in_ns1 =
            |entries: &[MemoryEntry]| entries.iter().filter(|e| e.namespace == "ns1").count();

        let before = mem.list(None, None).await.unwrap();
        let deleted = mem.purge_namespace("ns1").await.unwrap();
        let after = mem.list(None, None).await.unwrap();

        assert_eq!(in_ns1(&after), 0);
        assert_eq!(after.len() - in_ns1(&after), before.len() - in_ns1(&before));
        assert_eq!(deleted, in_ns1(&before));
    }

    #[tokio::test]
    async fn sqlite_purge_session_removes_all_matching_entries() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a1", "data1", MemoryCategory::Core, Some("sess-a"))
            .await
            .unwrap();
        mem.store("a2", "data2", MemoryCategory::Core, Some("sess-a"))
            .await
            .unwrap();
        mem.store("b1", "data3", MemoryCategory::Core, Some("sess-b"))
            .await
            .unwrap();

        let count = mem.purge_session("sess-a").await.unwrap();
        assert_eq!(count, 2);
        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn sqlite_purge_session_preserves_other_sessions() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a1", "data1", MemoryCategory::Core, Some("sess-a"))
            .await
            .unwrap();
        mem.store("b1", "data2", MemoryCategory::Core, Some("sess-b"))
            .await
            .unwrap();
        mem.store("c1", "data3", MemoryCategory::Core, None)
            .await
            .unwrap();

        let count = mem.purge_session("sess-a").await.unwrap();
        assert_eq!(count, 1);
        assert_eq!(mem.count().await.unwrap(), 2);

        let remaining = mem.list(None, None).await.unwrap();
        assert!(
            remaining
                .iter()
                .all(|e| e.session_id.as_deref() != Some("sess-a"))
        );
    }

    #[tokio::test]
    async fn sqlite_purge_session_returns_count() {
        let (_tmp, mem) = temp_sqlite();
        for i in 0..3 {
            mem.store(
                &format!("k{i}"),
                "data",
                MemoryCategory::Core,
                Some("target-sess"),
            )
            .await
            .unwrap();
        }

        let count = mem.purge_session("target-sess").await.unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn sqlite_purge_session_empty_session_is_noop() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "data", MemoryCategory::Core, Some("sess"))
            .await
            .unwrap();

        let count = mem.purge_session("").await.unwrap();
        assert_eq!(count, 0);
        assert_eq!(mem.count().await.unwrap(), 1);
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
        let results = mem
            .recall("fact", 10, Some("sess-a"), None, None)
            .await
            .unwrap();
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
        let results = mem.recall("fact", 10, None, None, None).await.unwrap();
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
        let results = mem
            .recall("secret", 10, Some("sess-b"), None, None)
            .await
            .unwrap();
        assert!(results.is_empty());

        // Session A can see its own data
        let results = mem
            .recall("secret", 10, Some("sess-a"), None, None)
            .await
            .unwrap();
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
        assert!(
            results
                .iter()
                .all(|e| e.session_id.as_deref() == Some("sess-a"))
        );

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
            let mem = SqliteMemory::new("test", tmp.path()).unwrap();
            mem.store("k1", "before reopen", MemoryCategory::Core, Some("sess-x"))
                .await
                .unwrap();
        }

        // Second open: migration runs again but is idempotent
        {
            let mem = SqliteMemory::new("test", tmp.path()).unwrap();
            let results = mem
                .recall("reopen", 10, Some("sess-x"), None, None)
                .await
                .unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].key, "k1");
            assert_eq!(results[0].session_id.as_deref(), Some("sess-x"));
        }
    }

    #[tokio::test]
    async fn schema_migration_tolerates_concurrent_initialization() {
        let tmp = TempDir::new().unwrap();

        // Seed an "old" DB that is missing the newer columns, so migrations have
        // real work to do when multiple initializers race.
        let db_path = tmp.path().join("memory").join("brain.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS memories (
                    id          TEXT PRIMARY KEY,
                    key         TEXT NOT NULL UNIQUE,
                    content     TEXT NOT NULL,
                    category    TEXT NOT NULL DEFAULT 'core',
                    embedding   BLOB,
                    created_at  TEXT NOT NULL,
                    updated_at  TEXT NOT NULL
                );",
            )
            .unwrap();
        }

        let workers = 12usize;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(workers));
        let mut handles = Vec::new();
        for _ in 0..workers {
            let dir = tmp.path().to_path_buf();
            let barrier = barrier.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                barrier.wait();
                SqliteMemory::new("test", &dir)
            }));
        }

        for h in handles {
            h.await.unwrap().unwrap();
        }

        // Ensure all expected columns exist after the concurrent migration.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(memories)").unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut cols = std::collections::HashSet::<String>::new();
        while let Some(row) = rows.next().unwrap() {
            cols.insert(row.get::<_, String>(1).unwrap());
        }

        assert!(cols.contains("session_id"));
        assert!(cols.contains("namespace"));
        assert!(cols.contains("importance"));
        assert!(cols.contains("superseded_by"));
    }

    // ── §4.1 Concurrent write contention tests ──────────────

    #[tokio::test]
    async fn sqlite_concurrent_writes_no_data_loss() {
        let (_tmp, mem) = temp_sqlite();
        let mem = std::sync::Arc::new(mem);

        let mut handles = Vec::new();
        for i in 0..10 {
            let mem = std::sync::Arc::clone(&mem);
            handles.push(zeroclaw_spawn::spawn!(async move {
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
            handles.push(zeroclaw_spawn::spawn!(async move {
                let _ = mem.get("shared_key").await.unwrap();
            }));
        }

        // Concurrent writes
        for i in 0..5 {
            let mem = std::sync::Arc::clone(&mem);
            handles.push(zeroclaw_spawn::spawn!(async move {
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

    // ── Export (GDPR Art. 20) tests ─────────────────────────

    #[tokio::test]
    async fn export_no_filter_returns_all_entries() {
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

        let filter = ExportFilter::default();
        let results = mem.export(&filter).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn export_with_namespace_filter() {
        let (_tmp, mem) = temp_sqlite();
        mem.store_with_metadata(
            "a",
            "ns1 data",
            MemoryCategory::Core,
            None,
            Some("ns1"),
            None,
        )
        .await
        .unwrap();
        mem.store_with_metadata(
            "b",
            "ns2 data",
            MemoryCategory::Core,
            None,
            Some("ns2"),
            None,
        )
        .await
        .unwrap();

        let filter = ExportFilter {
            namespace: Some("ns1".into()),
            ..Default::default()
        };
        let results = mem.export(&filter).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].namespace, "ns1");
    }

    #[tokio::test]
    async fn export_with_session_id_filter() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "sess-a data", MemoryCategory::Core, Some("sess-a"))
            .await
            .unwrap();
        mem.store("b", "sess-b data", MemoryCategory::Core, Some("sess-b"))
            .await
            .unwrap();

        let filter = ExportFilter {
            session_id: Some("sess-a".into()),
            ..Default::default()
        };
        let results = mem.export(&filter).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "a");
    }

    #[tokio::test]
    async fn export_with_category_filter() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("a", "core data", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "daily data", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let filter = ExportFilter {
            category: Some(MemoryCategory::Core),
            ..Default::default()
        };
        let results = mem.export(&filter).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].category, MemoryCategory::Core);
    }

    #[tokio::test]
    async fn export_with_time_range() {
        let (_tmp, mem) = temp_sqlite();
        // Store entries — created_at is set to Local::now() by store()
        mem.store("a", "old data", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b", "new data", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Export with a time range that covers everything
        let filter = ExportFilter {
            since: Some("2000-01-01T00:00:00Z".into()),
            until: Some("2099-12-31T23:59:59Z".into()),
            ..Default::default()
        };
        let results = mem.export(&filter).await.unwrap();
        assert_eq!(results.len(), 2);

        // Export with a time range in the far future (no results)
        let filter = ExportFilter {
            since: Some("2099-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        let results = mem.export(&filter).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn export_with_combined_filters() {
        let (_tmp, mem) = temp_sqlite();
        mem.store_with_metadata(
            "a",
            "match",
            MemoryCategory::Core,
            Some("sess-a"),
            Some("ns1"),
            None,
        )
        .await
        .unwrap();
        mem.store_with_metadata(
            "b",
            "no match ns",
            MemoryCategory::Core,
            Some("sess-a"),
            Some("ns2"),
            None,
        )
        .await
        .unwrap();
        mem.store_with_metadata(
            "c",
            "no match sess",
            MemoryCategory::Core,
            None,
            Some("ns1"),
            None,
        )
        .await
        .unwrap();

        let filter = ExportFilter {
            namespace: Some("ns1".into()),
            session_id: Some("sess-a".into()),
            category: Some(MemoryCategory::Core),
            since: Some("2000-01-01T00:00:00Z".into()),
            until: Some("2099-12-31T23:59:59Z".into()),
        };
        let results = mem.export(&filter).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "a");
    }

    #[tokio::test]
    async fn export_empty_database_returns_empty_vec() {
        let (_tmp, mem) = temp_sqlite();
        let filter = ExportFilter::default();
        let results = mem.export(&filter).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn export_ordering_is_chronological() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("first", "data1", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Small delay to ensure different timestamps
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        mem.store("second", "data2", MemoryCategory::Core, None)
            .await
            .unwrap();

        let filter = ExportFilter::default();
        let results = mem.export(&filter).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(
            results[0].timestamp <= results[1].timestamp,
            "Export must be ordered by created_at ASC"
        );
    }

    #[tokio::test]
    async fn export_preserves_field_integrity() {
        let (_tmp, mem) = temp_sqlite();
        mem.store_with_metadata(
            "roundtrip_key",
            "roundtrip content",
            MemoryCategory::Custom("custom_cat".into()),
            Some("sess-rt"),
            Some("ns-rt"),
            Some(0.9),
        )
        .await
        .unwrap();

        let filter = ExportFilter::default();
        let results = mem.export(&filter).await.unwrap();
        assert_eq!(results.len(), 1);
        let e = &results[0];
        assert_eq!(e.key, "roundtrip_key");
        assert_eq!(e.content, "roundtrip content");
        assert_eq!(e.category, MemoryCategory::Custom("custom_cat".into()));
        assert_eq!(e.session_id.as_deref(), Some("sess-rt"));
        assert_eq!(e.namespace, "ns-rt");
        assert_eq!(e.importance, Some(0.9));
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

    // ── SearchMode tests ─────────────────────────────────────────

    #[tokio::test]
    async fn search_mode_bm25_only() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::with_embedder(
            "test",
            tmp.path(),
            Arc::new(super::super::embeddings::NoopEmbedding),
            0.7,
            0.3,
            1000,
            None,
            SearchMode::Bm25,
        )
        .unwrap();
        mem.store(
            "lang",
            "User prefers Rust programming",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        mem.store("food", "User likes pizza", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = mem.recall("Rust", 10, None, None, None).await.unwrap();
        assert!(!results.is_empty(), "BM25 mode should find keyword matches");
        assert!(
            results.iter().any(|e| e.content.contains("Rust")),
            "BM25 should match on keyword 'Rust'"
        );
    }

    #[tokio::test]
    async fn search_mode_embedding_only() {
        let tmp = TempDir::new().unwrap();
        // NoopEmbedding returns None, so embedding-only mode will fall back to LIKE
        let mem = SqliteMemory::with_embedder(
            "test",
            tmp.path(),
            Arc::new(super::super::embeddings::NoopEmbedding),
            0.7,
            0.3,
            1000,
            None,
            SearchMode::Embedding,
        )
        .unwrap();
        mem.store(
            "lang",
            "User prefers Rust programming",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        // With NoopEmbedding, vector search returns empty, and FTS is skipped.
        // The recall method falls back to LIKE search.
        let results = mem.recall("Rust", 10, None, None, None).await.unwrap();
        // LIKE fallback should still find it
        assert!(
            results.iter().any(|e| e.content.contains("Rust")),
            "Embedding mode with noop should fall back to LIKE and still find results"
        );
    }

    #[tokio::test]
    async fn search_mode_hybrid_default() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        // Default search mode should be Hybrid
        assert_eq!(mem.search_mode, SearchMode::Hybrid);

        mem.store(
            "lang",
            "User prefers Rust programming",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let results = mem.recall("Rust", 10, None, None, None).await.unwrap();
        assert!(!results.is_empty(), "Hybrid mode should find results");
    }

    // Wires-crossed regression coverage. The user reported memory rows
    // returning the agents table UUID in `agent_alias` — the dashboard
    // then tried to route /config/agents/<uuid> and 404'd. These tests
    // assert the read path emits the resolved alias text in
    // `agent_alias` and keeps the raw UUID in `agent_id` so the
    // scoping wrapper still works.

    #[tokio::test]
    async fn get_returns_alias_text_in_agent_alias_and_uuid_in_agent_id() {
        let (_tmp, mem) = temp_sqlite();
        let alpha_uuid = mem.ensure_agent_uuid("clamps").await.unwrap();
        mem.store_with_agent(
            "row1",
            "v",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some(&alpha_uuid),
        )
        .await
        .unwrap();

        let entry = mem.get("row1").await.unwrap().expect("row1 must exist");
        assert_eq!(
            entry.agent_alias.as_deref(),
            Some("clamps"),
            "agent_alias must carry the human-readable alias, not the UUID"
        );
        assert_eq!(
            entry.agent_id.as_deref(),
            Some(alpha_uuid.as_str()),
            "agent_id must carry the raw UUID FK so scoping equality works"
        );
        assert_ne!(
            entry.agent_alias, entry.agent_id,
            "alias and id must differ on a SQL backend"
        );
    }

    #[tokio::test]
    async fn list_returns_alias_text_for_every_row() {
        let (_tmp, mem) = temp_sqlite();
        let a = mem.ensure_agent_uuid("clamps").await.unwrap();
        let b = mem.ensure_agent_uuid("glados").await.unwrap();
        for (key, owner) in [("r1", &a), ("r2", &b)] {
            mem.store_with_agent(
                key,
                "v",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(owner),
            )
            .await
            .unwrap();
        }

        let mut rows = mem.list(None, None).await.unwrap();
        rows.sort_by(|x, y| x.key.cmp(&y.key));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].agent_alias.as_deref(), Some("clamps"));
        assert_eq!(rows[1].agent_alias.as_deref(), Some("glados"));
        assert!(
            rows.iter().all(|r| r.agent_id.is_some()),
            "every row should carry agent_id"
        );
    }

    // ── session_id migration ──────────────────────────────────────

    #[tokio::test]
    async fn migrates_legacy_session_ids_to_sanitized_form() {
        let tmp = TempDir::new().unwrap();
        let raw_sid = "slack_C123_1.2_user one";
        let sanitized = sanitize_session_key(raw_sid);
        assert_ne!(
            raw_sid, sanitized,
            "test only meaningful when sanitization changes the value"
        );

        {
            let mem = SqliteMemory::new("test", tmp.path()).unwrap();
            mem.store(
                "legacy_key",
                "stored before sanitize fix",
                MemoryCategory::Conversation,
                Some(raw_sid),
            )
            .await
            .unwrap();
            let pre = mem.list(None, Some(raw_sid)).await.unwrap();
            assert_eq!(pre.len(), 1, "raw session_id should match before migration");
        }

        let mem = SqliteMemory::new("test", tmp.path()).unwrap();

        let by_sanitized = mem.list(None, Some(&sanitized)).await.unwrap();
        assert_eq!(
            by_sanitized.len(),
            1,
            "row must be discoverable via sanitized session_id"
        );
        assert_eq!(by_sanitized[0].key, "legacy_key");

        let by_raw = mem.list(None, Some(raw_sid)).await.unwrap();
        assert!(
            by_raw.is_empty(),
            "raw form must no longer match after migration"
        );
    }

    #[tokio::test]
    async fn session_id_migration_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let sanitized = sanitize_session_key("slack_C123_1.2_user");

        {
            let mem = SqliteMemory::new("test", tmp.path()).unwrap();
            mem.store("k", "v", MemoryCategory::Core, Some(&sanitized))
                .await
                .unwrap();
        }

        for _ in 0..3 {
            let mem = SqliteMemory::new("test", tmp.path()).unwrap();
            let entries = mem.list(None, Some(&sanitized)).await.unwrap();
            assert_eq!(entries.len(), 1);
        }
    }

    #[tokio::test]
    async fn session_id_migration_leaves_null_rows_untouched() {
        let tmp = TempDir::new().unwrap();

        {
            let mem = SqliteMemory::new("test", tmp.path()).unwrap();
            mem.store("global", "no session", MemoryCategory::Core, None)
                .await
                .unwrap();
        }

        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let entry = mem.get("global").await.unwrap().expect("row should exist");
        assert!(entry.session_id.is_none());
    }

    // ── §4.8 Issue #7694: storage-reader timestamp / ordering coverage ──
    //
    // These tests guard regressions in the storage reader's timestamp
    // loading and session-metadata ordering paths. They use neutral
    // fixture data only (no user-provided content) and rely on the
    // public `Memory` trait surface so they catch breakage at the
    // boundary a real caller would observe.

    /// Regression test for issue #7694: every recalled entry must expose
    /// a parseable RFC 3339 timestamp. A regression here would silently
    /// break UI rendering and time-windowed recall filters.
    #[tokio::test]
    async fn sqlite_timestamp_loading_is_rfc3339_round_trippable() {
        let (_tmp, mem) = temp_sqlite();
        mem.store(
            "ts-key-1",
            "content one",
            MemoryCategory::Core,
            Some("sess-7694"),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        mem.store(
            "ts-key-2",
            "content two",
            MemoryCategory::Core,
            Some("sess-7694"),
        )
        .await
        .unwrap();

        let entries = mem.list(None, Some("sess-7694")).await.unwrap();
        assert_eq!(entries.len(), 2);
        for entry in &entries {
            // RFC 3339 / ISO 8601 with timezone designator and millisecond
            // precision — chrono's default serialization. Anything else
            // would mean the schema or row mapper silently changed.
            let parsed =
                chrono::DateTime::parse_from_rfc3339(&entry.timestamp).unwrap_or_else(|err| {
                    panic!(
                        "entry {:?} returned non-RFC3339 timestamp {:?}: {err}",
                        entry.key, entry.timestamp
                    )
                });
            // Round-trip must preserve the original instant.
            assert_eq!(parsed.to_rfc3339(), entry.timestamp);
        }
    }

    /// Regression test for issue #7694: `list()` must return rows for a
    /// single session in stable `updated_at DESC` order so that the UI
    /// doesn't reshuffle rows on every refresh.
    #[tokio::test]
    async fn sqlite_session_metadata_ordering_is_stable_descending() {
        let (_tmp, mem) = temp_sqlite();
        // Seed with sleep gaps wide enough that updated_at strictly differs.
        // 50ms is well above the SQLite `created_at`/`updated_at` millisecond
        // resolution and stays comfortably under any reasonable CI time
        // budget; 15ms (the original value) was observed to flake on slow
        // shared runners where two adjacent writes landed within the same
        // millisecond bucket.
        let keys = ["ord-a", "ord-b", "ord-c", "ord-d"];
        for key in keys {
            mem.store(key, "body", MemoryCategory::Core, Some("sess-order"))
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // First read: capture the ordering.
        let first = mem.list(None, Some("sess-order")).await.unwrap();
        assert_eq!(first.len(), keys.len());
        let first_order: Vec<&str> = first.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            first_order,
            vec!["ord-d", "ord-c", "ord-b", "ord-a"],
            "list() must order rows by updated_at DESC (newest first)"
        );

        // Second read with no writes in between: order must be identical.
        let second = mem.list(None, Some("sess-order")).await.unwrap();
        let second_order: Vec<&str> = second.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            first_order, second_order,
            "ordering must be stable across reads"
        );

        // And every row must carry the session metadata we asked for.
        for entry in &first {
            assert_eq!(entry.session_id.as_deref(), Some("sess-order"));
        }
    }

    /// Regression test for issue #7694: when two rows in the same
    /// session share an `updated_at` boundary timestamp, `list()` must
    /// still return them deterministically (not randomly swap order on
    /// each read).
    ///
    /// Implementation note: `Local::now()` in `store()` carries
    /// nanosecond precision on this host (e.g. `…15.007463284+08:00`),
    /// so two back-to-back `store()` calls naturally land in distinct
    /// `updated_at` buckets and never tie. To exercise the tie path
    /// deterministically we seed the rows through the public `store()`
    /// API and then collapse both `updated_at` values to a single
    /// RFC 3339 timestamp via a direct SQL update through the public
    /// `connection()` accessor. The `list()` calls themselves still go
    /// through the public `Memory` trait surface.
    ///
    /// Scope note: this test verifies stable read-ordering when a tie
    /// has been forced. A query-level secondary sort key (e.g.
    /// `ORDER BY updated_at DESC, rowid ASC`) that would make
    /// tied-timestamp ordering *guaranteed* rather than
    /// implementation-defined is a production-logic change and is
    /// tracked separately — see PR #7921's follow-up notes for the
    /// reader-cursor side of the same family of issues.
    #[tokio::test]
    async fn sqlite_session_metadata_ordering_ties_are_deterministic() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("tie-x", "x", MemoryCategory::Core, Some("sess-tie"))
            .await
            .unwrap();
        mem.store("tie-y", "y", MemoryCategory::Core, Some("sess-tie"))
            .await
            .unwrap();

        // Force both rows to share the exact same `created_at` /
        // `updated_at` value. Without this, two back-to-back `store()`
        // calls on this host produce distinct nanosecond timestamps
        // and the test would never exercise the tie path. We pin both
        // columns because `list()` exposes `m.created_at` as the
        // entry's `timestamp` while ordering by `m.updated_at`.
        let tied_ts = "2026-06-19T00:00:00.000000000+00:00";
        {
            let conn = mem.connection().lock();
            conn.execute(
                "UPDATE memories SET created_at = ?1, updated_at = ?1 \
                 WHERE key IN (?2, ?3)",
                rusqlite::params![tied_ts, "tie-x", "tie-y"],
            )
            .unwrap();
        }

        let first = mem.list(None, Some("sess-tie")).await.unwrap();
        assert_eq!(first.len(), 2);

        // Lock in that a tie really occurred. Without this, the test
        // degrades into a generic "stable order" check and the
        // function name overstates what it covers.
        assert_eq!(
            first[0].timestamp, first[1].timestamp,
            "expected both rows to share the forced updated_at"
        );
        assert_eq!(first[0].timestamp, tied_ts);

        // Capture the order once.
        let snapshot: Vec<String> = first.iter().map(|e| e.key.clone()).collect();

        // Five more reads must all agree with the snapshot. If ordering
        // were non-deterministic at a tied timestamp, this would flake.
        for _ in 0..5 {
            let again = mem.list(None, Some("sess-tie")).await.unwrap();
            let again_keys: Vec<String> = again.iter().map(|e| e.key.clone()).collect();
            assert_eq!(
                again_keys, snapshot,
                "list() must yield a deterministic order across reads"
            );
        }
    }
}
