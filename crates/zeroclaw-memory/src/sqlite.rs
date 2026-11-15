pub(crate) mod key_scope;

use super::embeddings::EmbeddingProvider;
use super::traits::{
    ExportFilter, Memory, MemoryCategory, MemoryEntry, MemoryStats, StoreOptions,
    is_recent_recall_query,
};
use super::vector;
use anyhow::Context;
use async_trait::async_trait;
use chrono::Local;
use parking_lot::{Mutex, RwLock};
use rusqlite::{Connection, params};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::sync::{Mutex as StdMutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;
use zeroclaw_api::session_keys::sanitize_session_key;
use zeroclaw_config::schema::SearchMode;

/// Maximum allowed open timeout (seconds) to avoid unreasonable waits.
const SQLITE_OPEN_TIMEOUT_CAP_SECS: u64 = 300;
const SQLITE_OPEN_LOCK_RETRY_BUDGET: Duration = Duration::from_secs(5);
static SQLITE_MEMORY_STARTUP_LOCK: StdMutex<()> = StdMutex::new(());

fn acquire_sqlite_startup_lock() -> MutexGuard<'static, ()> {
    SQLITE_MEMORY_STARTUP_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn is_db_locked_error(error: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode;
    matches!(
        error,
        rusqlite::Error::SqliteFailure(err, _)
            if matches!(err.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// SQLite memory with agent/key identity for global rows and agent/session/key
/// identity for session-bound Conversation rows. New scoped rows use reversible
/// physical keys; public entries expose logical keys. Legacy rows stay in place.
///
/// Point get/forget calls inside `TOOL_LOOP_MEMORY_SESSIONS` resolve only the
/// first canonical session (or global facts when no session row matches).
/// Empty installed context hides Conversation rows. Calls outside that host
/// context retain the historical administrative, session-unfiltered behavior.
#[derive(Clone)]
pub struct SqliteMemory {
    alias: String,
    conn: Arc<Mutex<Connection>>,
    embedder: Arc<RwLock<Arc<dyn EmbeddingProvider>>>,
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
        crate::migration::migrate_sqlite_memory_to_v3(&db_path, &conn)?;
        Self::init_schema(&conn)?;
        Ok(Self {
            alias: alias.to_string(),
            conn: Arc::new(Mutex::new(conn)),
            embedder: Arc::new(RwLock::new(Arc::new(super::embeddings::NoopEmbedding))),
            vector_weight: 0.7,
            keyword_weight: 0.3,
            cache_max: 10_000,
            search_mode: SearchMode::default(),
        })
    }

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

        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA mmap_size    = 8388608;
             PRAGMA cache_size   = -2000;
             PRAGMA temp_store   = MEMORY;",
        )?;

        Self::init_schema(&conn)?;
        crate::migration::migrate_sqlite_memory_to_v3(&db_path, &conn)?;
        Self::init_schema(&conn)?;

        Ok(Self {
            alias: alias.to_string(),
            conn: Arc::new(Mutex::new(conn)),
            embedder: Arc::new(RwLock::new(embedder)),
            vector_weight,
            keyword_weight,
            cache_max,
            search_mode,
        })
    }

    /// Open SQLite with bounded lock retries and an optional caller timeout.
    /// Native opens already in progress cannot be cancelled; the deadline
    /// prevents a delayed worker or backoff from starting a later attempt.
    fn open_connection(
        db_path: &Path,
        open_timeout_secs: Option<u64>,
    ) -> anyhow::Result<Connection> {
        let path_buf = db_path.to_path_buf();

        let conn = if let Some(secs) = open_timeout_secs {
            let capped = secs.min(SQLITE_OPEN_TIMEOUT_CAP_SECS);
            let timeout = Duration::from_secs(capped);
            let deadline = Instant::now() + timeout.min(SQLITE_OPEN_LOCK_RETRY_BUDGET);
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let result = Self::open_connection_retry(&path_buf, deadline);
                let _ = tx.send(result);
            });
            match rx.recv_timeout(timeout) {
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
            Self::open_connection_retry(&path_buf, Instant::now() + SQLITE_OPEN_LOCK_RETRY_BUDGET)
                .context("SQLite failed to open database")?
        };

        Ok(conn)
    }

    fn open_connection_retry(db_path: &Path, deadline: Instant) -> anyhow::Result<Connection> {
        // Kernel SQLite auto-extensions are process-wide and can read schema
        // during open, before rusqlite installs its normal busy handler.
        // Retry only the failed connection open; never replay schema changes.
        let mut backoff = Duration::from_millis(10);
        loop {
            anyhow::ensure!(
                Instant::now() < deadline,
                "SQLite open deadline expired before connection attempt"
            );
            match Connection::open(db_path) {
                Ok(conn) => return Ok(conn),
                Err(error) if is_db_locked_error(&error) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(error.into());
                    }
                    thread::sleep(backoff.min(remaining));
                    if Instant::now() >= deadline {
                        return Err(error.into());
                    }
                    backoff = (backoff * 2).min(Duration::from_millis(250));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Initialize all tables: memories, FTS5, `embedding_cache`
    fn init_schema(conn: &Connection) -> anyhow::Result<()> {
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
            -- migration in `crate::migration::migrate_sqlite_memory_to_v3`
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
            CREATE INDEX IF NOT EXISTS idx_cache_accessed ON embedding_cache(accessed_at);

            -- Store-level metadata (e.g. the embedding identity that produced
            -- the stored vectors). Sits beside schema_version, which is keyed
            -- by component with an INTEGER version and can't carry strings.
            CREATE TABLE IF NOT EXISTS memory_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
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
        add_memories_column_if_missing(conn, "kind", "ALTER TABLE memories ADD COLUMN kind TEXT;")?;
        add_memories_column_if_missing(
            conn,
            "pinned",
            "ALTER TABLE memories ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;",
        )?;
        add_memories_column_if_missing(
            conn,
            "tenant_id",
            "ALTER TABLE memories ADD COLUMN tenant_id TEXT;",
        )?;
        execute_batch_retry(
            conn,
            "CREATE INDEX IF NOT EXISTS idx_memories_namespace_category ON memories(namespace, category);",
        )
        .with_context(|| "SQLite init_schema failed: CREATE INDEX idx_memories_namespace_category")?;

        Self::migrate_session_ids_to_sanitized(conn)?;

        Ok(())
    }

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

    async fn store_row_with_metadata(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        options: StoreOptions,
        agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        let key = key.to_string();
        let content = content.to_string();
        let sid = session_id.map(String::from);
        let ns = options.namespace.unwrap_or_else(|| "default".to_string());

        // Storage-level reservation for the Soul key space: rows under the
        // reserved prefix exist only in the reserved namespace, and the
        // reserved namespace accepts only reserved-prefix keys. Ambient
        // stores (namespace "default") can therefore never upsert-overwrite
        // a Soul row through the (agent_id, key) conflict target, and a
        // Soul-namespace write can never smuggle an ambient key shape.
        if ns == crate::SOUL_NAMESPACE {
            if !key.starts_with(crate::SOUL_KEY_PREFIX) {
                anyhow::bail!(
                    "refused: namespace '{}' requires a key with the reserved '{}' prefix",
                    crate::SOUL_NAMESPACE,
                    crate::SOUL_KEY_PREFIX
                );
            }
        } else if key.starts_with(crate::SOUL_KEY_PREFIX) {
            anyhow::bail!(
                "refused: key prefix '{}' is reserved for the Soul namespace",
                crate::SOUL_KEY_PREFIX
            );
        }

        // Reserved Soul content is local-only on implicit provider paths.
        // Validate its reservation above before any embedding request.
        let embedding_bytes = if ns == crate::SOUL_NAMESPACE {
            None
        } else {
            match self.get_or_compute_embedding(&content).await {
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
            }
        };

        let imp = options.importance.unwrap_or(0.5);
        let kind = options
            .kind
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let pinned = i64::from(options.pinned);
        let tenant_id = options.tenant_id;
        let aid = agent_id.map(String::from);

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut guard = conn.lock();
            let conn = guard.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let now = Local::now().to_rfc3339();
            let cat = Self::category_to_str(&category);
            let agent: String = match aid {
                Some(agent) => agent,
                None => conn.query_row(
                    "SELECT id FROM agents WHERE alias = 'default' LIMIT 1",
                    [],
                    |row| row.get(0),
                )?,
            };
            let conversation_session = if cat == "conversation" {
                sid.as_deref()
            } else {
                None
            };
            let existing = key_scope::rows(&conn, &key, Some(&agent))?
                .into_iter()
                .find(|row| row.conversation_session() == conversation_session);
            let (id, physical_key) = if let Some(row) = existing {
                (row.id, row.physical)
            } else {
                let occupied: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM memories WHERE agent_id=?1 AND key=?2)",
                    params![agent, key],
                    |row| row.get(0),
                )?;
                if conversation_session.is_some() || occupied {
                    loop {
                        let encoded = key_scope::encode(&key, conversation_session)?;
                        let occupied: bool = conn.query_row(
                            "SELECT EXISTS(SELECT 1 FROM memories WHERE agent_id=?1 AND key=?2)",
                            params![agent, encoded.1],
                            |row| row.get(0),
                        )?;
                        if !occupied {
                            break encoded;
                        }
                    }
                } else {
                    (Uuid::new_v4().to_string(), key.clone())
                }
            };

            conn.execute(
                "INSERT INTO memories (
                    id, key, content, category, embedding, created_at, updated_at,
                    session_id, namespace, importance, agent_id, kind, pinned, tenant_id
                 )
                 VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                    COALESCE(?11, (SELECT id FROM agents WHERE alias = 'default' LIMIT 1)),
                    ?12, ?13, ?14
                 )
                 ON CONFLICT(agent_id, key) DO UPDATE SET
                    content = excluded.content,
                    category = excluded.category,
                    embedding = excluded.embedding,
                    updated_at = excluded.updated_at,
                    session_id = excluded.session_id,
                    namespace = excluded.namespace,
                    importance = excluded.importance,
                    kind = excluded.kind,
                    pinned = excluded.pinned,
                    tenant_id = excluded.tenant_id",
                params![
                    id,
                    physical_key,
                    content,
                    cat,
                    embedding_bytes,
                    now,
                    now,
                    sid,
                    ns,
                    imp,
                    agent,
                    kind,
                    pinned,
                    tenant_id
                ],
            )?;
            conn.commit()?;
            Ok(())
        })
        .await?
    }

    async fn forget_key(&self, key: &str, agent: Option<&str>) -> anyhow::Result<bool> {
        let scope = key_scope::PointScope::current();
        let conn = self.conn.clone();
        let key = key.to_string();
        let agent = agent.map(str::to_string);
        tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let mut guard = conn.lock();
            let conn = guard.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let rows = scope.select(
                key_scope::rows(&conn, &key, agent.as_deref())?,
                agent.is_some(),
            );
            let mut affected = 0;
            for row in rows {
                affected += conn.execute("DELETE FROM memories WHERE id = ?1", params![row.id])?;
            }
            conn.commit()?;
            Ok(affected > 0)
        })
        .await?
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

    /// The categories whose session-NULL rows are durable global knowledge
    /// (see [`Self::is_durable_global_row`]). Single source of truth for
    /// the carve-out: the SQL predicate in [`Self::vector_search`] derives
    /// its bind parameters from this slice via `category_to_str`, so the
    /// set is never spelled twice.
    const DURABLE_GLOBAL_CATEGORIES: [MemoryCategory; 2] =
        [MemoryCategory::Core, MemoryCategory::Daily];

    /// Whether a row is durable global knowledge: a `core`/`daily` row with
    /// no session binding is long-term knowledge meant to be recallable
    /// from any session, not a per-session artifact. Rows that DO carry a
    /// session binding (consolidation keeps a survivor's `session_id` even
    /// on `core` rows) stay session-scoped, as do `conversation` and custom
    /// categories.
    fn is_durable_global_row(category: &MemoryCategory, session_id: Option<&str>) -> bool {
        session_id.is_none() && Self::DURABLE_GLOBAL_CATEGORIES.contains(category)
    }

    fn decode_kind(raw: Option<String>) -> Option<super::traits::MemoryKind> {
        raw.and_then(|kind| serde_json::from_str(&kind).ok())
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

    pub fn stored_embedding_identity(
        &self,
    ) -> anyhow::Result<Option<super::embeddings::EmbeddingIdentity>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT key, value FROM memory_meta WHERE key IN \
             ('embedding_provider', 'embedding_model', 'embedding_dimensions')",
        )?;
        let mut provider = None;
        let mut model = None;
        let mut dimensions = None;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            match key.as_str() {
                "embedding_provider" => provider = Some(value),
                "embedding_model" => model = Some(value),
                "embedding_dimensions" => dimensions = value.parse::<usize>().ok(),
                _ => {}
            }
        }
        Ok(match (provider, model, dimensions) {
            (Some(provider), Some(model), Some(dimensions)) => {
                Some(super::embeddings::EmbeddingIdentity {
                    provider,
                    model,
                    dimensions,
                })
            }
            _ => None,
        })
    }

    /// Record `identity` in `memory_meta` without touching any vectors.
    /// Used to adopt the current identity on stores that predate identity
    /// tracking, and after a match check confirms nothing changed.
    pub fn record_embedding_identity(
        &self,
        identity: &super::embeddings::EmbeddingIdentity,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        Self::write_identity_rows(&tx, identity)?;
        tx.commit()?;
        Ok(())
    }

    pub fn invalidate_embeddings_for_identity_change(
        &self,
        new_identity: &super::embeddings::EmbeddingIdentity,
    ) -> anyhow::Result<usize> {
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        let invalidated = tx.execute(
            "UPDATE memories SET embedding = NULL WHERE embedding IS NOT NULL",
            [],
        )?;
        tx.execute("DELETE FROM embedding_cache", [])?;
        Self::write_identity_rows(&tx, new_identity)?;
        tx.commit()?;
        Ok(invalidated)
    }

    fn write_identity_rows(
        conn: &Connection,
        identity: &super::embeddings::EmbeddingIdentity,
    ) -> anyhow::Result<()> {
        let mut stmt =
            conn.prepare("INSERT OR REPLACE INTO memory_meta (key, value) VALUES (?1, ?2)")?;
        stmt.execute(params!["embedding_provider", identity.provider])?;
        stmt.execute(params!["embedding_model", identity.model])?;
        stmt.execute(params![
            "embedding_dimensions",
            identity.dimensions.to_string()
        ])?;
        Ok(())
    }

    /// Get embedding from cache, or compute + cache it
    pub async fn get_or_compute_embedding(&self, text: &str) -> anyhow::Result<Option<Vec<f32>>> {
        // Snapshot the embedder once so a concurrent `refresh_embedder` swap
        // can't split this call across two providers; the guard is dropped
        // immediately, never held across the `.await` below.
        let embedder = self.embedder.read().clone();
        if embedder.dimensions() == 0 {
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
        let embedding = embedder.embed_one(text).await?;
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
        Self::fts5_search_scoped(conn, query, limit, None, None, None)
    }

    fn fts5_search_for_session_and_agents(
        conn: &Connection,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        allowed_agent_ids: &[String],
        namespace: Option<&str>,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        Self::fts5_search_scoped(
            conn,
            query,
            limit,
            session_id,
            Some(allowed_agent_ids),
            namespace,
        )
    }

    fn fts5_search_scoped(
        conn: &Connection,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        allowed_agent_ids: Option<&[String]>,
        namespace: Option<&str>,
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

        let mut sql = "SELECT m.id, bm25(memories_fts) as score
                       FROM memories_fts f
                       JOIN memories m ON m.rowid = f.rowid
                       WHERE memories_fts MATCH ?1"
            .to_string();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(fts_query)];
        let mut param_idx = 2;

        // Namespace scope: `None` is the ambient recall surface, which
        // structurally excludes the reserved Soul namespace; `Some(ns)`
        // restricts to exactly that namespace (the explicit opt-in read
        // channel used by `recall_namespaced`).
        match namespace {
            Some(ns) => {
                let _ = write!(sql, " AND m.namespace = ?{param_idx}");
                param_values.push(Box::new(ns.to_string()));
                param_idx += 1;
            }
            None => {
                let _ = write!(
                    sql,
                    " AND (m.namespace IS NULL OR m.namespace != '{}')",
                    crate::SOUL_NAMESPACE
                );
            }
        }

        if let Some(sid) = session_id {
            let category_placeholders = Self::DURABLE_GLOBAL_CATEGORIES
                .iter()
                .enumerate()
                .map(|(offset, _)| format!("?{}", param_idx + 1 + offset))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(
                sql,
                " AND (m.session_id = ?{param_idx} OR \
                 (m.session_id IS NULL AND m.category IN ({category_placeholders})))"
            );
            param_values.push(Box::new(sid.to_string()));
            for category in &Self::DURABLE_GLOBAL_CATEGORIES {
                param_values.push(Box::new(Self::category_to_str(category)));
            }
            param_idx += 1 + Self::DURABLE_GLOBAL_CATEGORIES.len();
        }
        if let Some(allowed_agent_ids) = allowed_agent_ids
            && !allowed_agent_ids.is_empty()
        {
            let agent_placeholders = (0..allowed_agent_ids.len())
                .map(|offset| format!("?{}", param_idx + offset))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(sql, " AND m.agent_id IN ({agent_placeholders})");
            for agent_id in allowed_agent_ids {
                param_values.push(Box::new(agent_id.clone()));
            }
            param_idx += allowed_agent_ids.len();
        }

        let _ = write!(sql, " ORDER BY score LIMIT ?{param_idx}");
        #[allow(clippy::cast_possible_wrap)]
        let limit_i64 = limit as i64;
        param_values.push(Box::new(limit_i64));
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(AsRef::as_ref).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
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
    /// Optional `category` and `session_id` filters reduce full-table scans
    /// when the caller already knows the scope of relevant memories.
    ///
    /// A `session_id` filter still admits durable global rows (see
    /// `Self::is_durable_global_row`): global `core`/`daily` facts must be
    /// semantically recallable from sessions that did not write them, while
    /// session-bound rows from other sessions stay excluded.
    pub fn vector_search(
        conn: &Connection,
        query_embedding: &[f32],
        limit: usize,
        category: Option<&str>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        Self::vector_search_scoped(
            conn,
            query_embedding,
            limit,
            category,
            session_id,
            None,
            None,
        )
    }

    fn vector_search_for_agents(
        conn: &Connection,
        query_embedding: &[f32],
        limit: usize,
        category: Option<&str>,
        session_id: Option<&str>,
        allowed_agent_ids: &[String],
        namespace: Option<&str>,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        Self::vector_search_scoped(
            conn,
            query_embedding,
            limit,
            category,
            session_id,
            Some(allowed_agent_ids),
            namespace,
        )
    }

    fn vector_search_scoped(
        conn: &Connection,
        query_embedding: &[f32],
        limit: usize,
        category: Option<&str>,
        session_id: Option<&str>,
        allowed_agent_ids: Option<&[String]>,
        namespace: Option<&str>,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        let mut sql = "SELECT id, embedding FROM memories WHERE embedding IS NOT NULL".to_string();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut idx = 1;

        // See fts5_search_scoped: `None` is the ambient surface (Soul
        // namespace excluded), `Some(ns)` restricts to that namespace.
        match namespace {
            Some(ns) => {
                let _ = write!(sql, " AND namespace = ?{idx}");
                param_values.push(Box::new(ns.to_string()));
                idx += 1;
            }
            None => {
                let _ = write!(
                    sql,
                    " AND (namespace IS NULL OR namespace != '{}')",
                    crate::SOUL_NAMESPACE
                );
            }
        }

        if let Some(cat) = category {
            let _ = write!(sql, " AND category = ?{idx}");
            param_values.push(Box::new(cat.to_string()));
            idx += 1;
        }
        if let Some(sid) = session_id {
            let category_placeholders = Self::DURABLE_GLOBAL_CATEGORIES
                .iter()
                .enumerate()
                .map(|(offset, _)| format!("?{}", idx + 1 + offset))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(
                sql,
                " AND (session_id = ?{idx} OR (session_id IS NULL AND category IN ({category_placeholders})))"
            );
            param_values.push(Box::new(sid.to_string()));
            for category in &Self::DURABLE_GLOBAL_CATEGORIES {
                param_values.push(Box::new(Self::category_to_str(category)));
            }
            idx += 1 + Self::DURABLE_GLOBAL_CATEGORIES.len();
        }
        if let Some(allowed_agent_ids) = allowed_agent_ids
            && !allowed_agent_ids.is_empty()
        {
            let agent_placeholders = (0..allowed_agent_ids.len())
                .map(|offset| format!("?{}", idx + offset))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(sql, " AND agent_id IN ({agent_placeholders})");
            for agent_id in allowed_agent_ids {
                param_values.push(Box::new(agent_id.clone()));
            }
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
        namespace: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let conn = self.conn.clone();
        let sid = session_id.map(String::from);
        let since_owned = since.map(String::from);
        let until_owned = until.map(String::from);
        let ns_owned = namespace.map(String::from);

        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let conn = conn.lock();
            let since_ref = since_owned.as_deref();
            let until_ref = until_owned.as_deref();

            let mut sql =
                "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, m.kind, m.pinned, a.alias, m.agent_id, m.tenant_id \
                 FROM memories m LEFT JOIN agents a ON a.id = m.agent_id \
                 WHERE m.superseded_by IS NULL AND 1=1"
                    .to_string();
            let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            let mut idx = 1;

            // See fts5_search_scoped: `None` is the ambient surface (Soul
            // namespace excluded), `Some(ns)` restricts to that namespace.
            match ns_owned.as_deref() {
                Some(ns) => {
                    let _ = write!(sql, " AND m.namespace = ?{idx}");
                    param_values.push(Box::new(ns.to_string()));
                    idx += 1;
                }
                None => {
                    let _ = write!(
                        sql,
                        " AND (m.namespace IS NULL OR m.namespace != '{}')",
                        crate::SOUL_NAMESPACE
                    );
                }
            }

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
                    key: key_scope::row_key(row)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    kind: Self::decode_kind(row.get(9)?),
                    pinned: row.get::<_, i64>(10)? != 0,
                    tenant_id: row.get(13)?,
                    agent_alias: row.get(11)?,
                    agent_id: row.get(12)?,
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

    async fn recall_scoped(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
        allowed_agent_ids: Option<Vec<String>>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.recall_scoped_with_namespace(
            query,
            limit,
            session_id,
            since,
            until,
            allowed_agent_ids,
            None,
        )
        .await
    }

    /// The recall pipeline with an explicit namespace scope. `None` is the
    /// ambient surface (reserved Soul namespace excluded from every search
    /// channel); `Some(ns)` is the namespaced opt-in channel and restricts
    /// every search channel to exactly that namespace.
    async fn recall_scoped_with_namespace(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
        allowed_agent_ids: Option<Vec<String>>,
        namespace: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let allowed_agent_ids = allowed_agent_ids.unwrap_or_default();
        // Time-only query: list by time range when no keywords.
        // Treat only a bare "*" as the same recent-entry request; keep
        // real wildcard searches such as "wild*" on the keyword path.
        if is_recent_recall_query(query) {
            let recall_limit = if allowed_agent_ids.is_empty() {
                limit
            } else {
                self.count().await?.max(limit)
            };
            let raw = self
                .recall_by_time_only(recall_limit, session_id, since, until, namespace)
                .await?;
            if allowed_agent_ids.is_empty() {
                return Ok(raw);
            }
            return Ok(raw
                .into_iter()
                .filter(|entry| {
                    entry
                        .agent_id
                        .as_deref()
                        .is_some_and(|agent_id| allowed_agent_ids.iter().any(|id| id == agent_id))
                })
                .take(limit)
                .collect());
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
        let ns_owned = namespace.map(String::from);
        let vector_weight = self.vector_weight;
        let keyword_weight = self.keyword_weight;
        let search_mode = self.search_mode.clone();
        let allowed = allowed_agent_ids;

        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let conn = conn.lock();
            let session_ref = sid.as_deref();
            let since_ref = since_owned.as_deref();
            let until_ref = until_owned.as_deref();
            let ns_ref = ns_owned.as_deref();
            let agent_filter = if allowed.is_empty() {
                None
            } else {
                Some(allowed.as_slice())
            };
            // The vector stage is live only when an embedder produced a query
            // vector; it selects the scoped FTS variant. The BM25-only path
            // (stock `embedding_provider = "none"` => Noop embedder, and
            // explicit `search_mode = "bm25"`) keeps its strict session filter.
            let vector_live = query_embedding.is_some();

            // FTS5 BM25 keyword search (skip for embedding-only mode)
            let keyword_results = if search_mode == SearchMode::Embedding {
                Vec::new()
            } else if let Some(agent_filter) = agent_filter {
                if vector_live {
                    Self::fts5_search_for_session_and_agents(
                        &conn,
                        &query,
                        limit * 2,
                        session_ref,
                        agent_filter,
                        ns_ref,
                    )
                    .unwrap_or_default()
                } else {
                    Self::fts5_search_scoped(
                        &conn,
                        &query,
                        limit * 2,
                        None,
                        Some(agent_filter),
                        ns_ref,
                    )
                    .unwrap_or_default()
                }
            } else if vector_live {
                Self::fts5_search_scoped(&conn, &query, limit * 2, session_ref, None, ns_ref)
                    .unwrap_or_default()
            } else {
                Self::fts5_search_scoped(&conn, &query, limit * 2, None, None, ns_ref)
                    .unwrap_or_default()
            };

            // Vector similarity search (skip for BM25-only mode)
            let vector_results = if search_mode == SearchMode::Bm25 {
                Vec::new()
            } else if let Some(ref qe) = query_embedding {
                if let Some(agent_filter) = agent_filter {
                    Self::vector_search_for_agents(
                        &conn,
                        qe,
                        limit * 2,
                        None,
                        session_ref,
                        agent_filter,
                        ns_ref,
                    )
                    .unwrap_or_default()
                } else {
                    Self::vector_search_scoped(
                        &conn,
                        qe,
                        limit * 2,
                        None,
                        session_ref,
                        None,
                        ns_ref,
                    )
                    .unwrap_or_default()
                }
            } else {
                Vec::new()
            };

            // Merge results based on search mode
            let merged = if vector_results.is_empty() {
                // FTS-only survivors: map raw BM25 onto the [0, 1] axis
                // (matching hybrid_merge's internal keyword normalization) so
                // downstream relevance thresholding and the injection rerank
                // stage see one calibrated scale, whether or not the vector
                // stage is live. Batch-max normalization; the strict session
                // filter still applies below.
                crate::normalize::bm25_to_unit(&keyword_results)
                    .into_iter()
                    .map(|(id, score)| vector::ScoredResult {
                        id,
                        vector_score: None,
                        keyword_score: Some(score),
                        final_score: score,
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
                    "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, m.kind, m.pinned, a.alias, m.agent_id, m.tenant_id \
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
                        row.get::<_, i64>(10)? != 0,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                    ))
                })?;

                let mut entry_map = std::collections::HashMap::new();
                for row in rows {
                    let (
                        id,
                        key,
                        content,
                        cat,
                        ts,
                        sid,
                        ns,
                        imp,
                        sup,
                        kind,
                        pinned,
                        alias,
                        aid,
                        tenant,
                    ) = row?;
                    entry_map.insert(
                        id,
                        (
                            key, content, cat, ts, sid, ns, imp, sup, kind, pinned, alias, aid,
                            tenant,
                        ),
                    );
                }

                for scored in &merged {
                    if let Some((
                        key,
                        content,
                        cat,
                        ts,
                        sid,
                        ns,
                        imp,
                        sup,
                        kind,
                        pinned,
                        alias,
                        aid,
                        tenant,
                    )) = entry_map.remove(&scored.id)
                    {
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
                            key: key_scope::decode(&scored.id, key),
                            content,
                            category: Self::str_to_category(&cat),
                            timestamp: ts,
                            session_id: sid,
                            score: Some(f64::from(scored.final_score)),
                            namespace: ns.unwrap_or_else(|| "default".into()),
                            importance: imp,
                            superseded_by: sup,
                            kind: Self::decode_kind(kind),
                            pinned,
                            tenant_id: tenant,
                            agent_alias: alias,
                            agent_id: aid,
                        };
                        // Session filter for the hybrid stage. With a live
                        // vector stage, durable global rows are exempt so
                        // they reach recall from any session, whichever
                        // stage (vector or keyword) surfaced them; the
                        // BM25-only path keeps the strict legacy filter.
                        if let Some(filter_sid) = session_ref
                            && entry.session_id.as_deref() != Some(filter_sid)
                            && !(vector_live
                                && Self::is_durable_global_row(
                                    &entry.category,
                                    entry.session_id.as_deref(),
                                ))
                        {
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
                    let mut agent_conditions = String::new();
                    if let Some(agent_filter) = agent_filter {
                        let agent_placeholders = (0..agent_filter.len())
                            .map(|offset| format!("?{}", param_idx + offset))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let _ = write!(agent_conditions, " AND m.agent_id IN ({agent_placeholders})");
                        param_idx += agent_filter.len();
                    }
                    // The LIKE fallback carries the same namespace scope
                    // as the FTS/vector stages it backs up: ambient recall
                    // excludes the reserved Soul namespace, a namespaced
                    // recall restricts to it.
                    let (namespace_condition, namespace_param) = match ns_ref {
                        Some(ns) => {
                            let placeholder = format!(" AND m.namespace = ?{param_idx}");
                            param_idx += 1;
                            (placeholder, Some(ns.to_string()))
                        }
                        None => (
                            format!(
                                " AND (m.namespace IS NULL OR m.namespace != '{}')",
                                crate::SOUL_NAMESPACE
                            ),
                            None,
                        ),
                    };
                    let sql = format!(
                        "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, m.kind, m.pinned, a.alias, m.agent_id, m.tenant_id
                         FROM memories m LEFT JOIN agents a ON a.id = m.agent_id
                         WHERE m.superseded_by IS NULL AND ({where_clause}){time_conditions}{agent_conditions}{namespace_condition}
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
                    if let Some(agent_filter) = agent_filter {
                        for agent_id in agent_filter {
                            param_values.push(Box::new(agent_id.clone()));
                        }
                    }
                    if let Some(ns_param) = namespace_param {
                        param_values.push(Box::new(ns_param));
                    }
                    #[allow(clippy::cast_possible_wrap)]
                    param_values.push(Box::new(sql_limit as i64));
                    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                        param_values.iter().map(AsRef::as_ref).collect();
                    let rows = stmt.query_map(params_ref.as_slice(), |row| {
                        Ok(MemoryEntry {
                            id: row.get(0)?,
                            key: key_scope::row_key(row)?,
                            content: row.get(2)?,
                            category: Self::str_to_category(&row.get::<_, String>(3)?),
                            timestamp: row.get(4)?,
                            session_id: row.get(5)?,
                            score: Some(1.0),
                            namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                            importance: row.get(7)?,
                            superseded_by: row.get(8)?,
                            kind: Self::decode_kind(row.get(9)?),
                            pinned: row.get::<_, i64>(10)? != 0,
                            tenant_id: row.get(13)?,
                            agent_alias: row.get(11)?,
                            agent_id: row.get(12)?,
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

    /// Replace the live embedder in place. Shared by the runtime
    /// `refresh_embedder` hook (after a `config/set` provider-profile change)
    /// and tests that need to inject a fake embedder. Existing `Arc<dyn Memory>`
    /// holders observe the new embedder on their next embed without rebuilding
    /// the handle.
    pub(crate) fn swap_embedder(&self, embedder: Arc<dyn EmbeddingProvider>) {
        *self.embedder.write() = embedder;
        // `embedding_cache` is keyed by content hash only, so every cached
        // vector belongs to the *previous* provider/model/dimensions. Drop the
        // cache on swap so the next embed goes through the new embedder instead
        // of returning a stale vector. Best-effort - a cache-clear failure must
        // not block the swap.
        if let Err(e) = self.conn.lock().execute("DELETE FROM embedding_cache", []) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                "memory embedder refresh: failed to clear stale embedding cache"
            );
        }
    }

    /// Dimensions of the currently-installed embedder (0 = Noop / no vectors).
    /// Cheap read-only diagnostic; lets callers confirm a live embedder refresh
    /// took effect after a `config/set` provider-profile change.
    pub fn embedder_dimensions(&self) -> usize {
        self.embedder.read().dimensions()
    }
}

#[async_trait]
impl Memory for SqliteMemory {
    fn name(&self) -> &str {
        "sqlite"
    }

    fn refresh_embedder(
        &self,
        model_provider: &str,
        api_key: Option<&str>,
        model: &str,
        dimensions: usize,
    ) {
        // Rebuild from the freshly-resolved settings and swap in place. No
        // provider state is duplicated into a separate cache — the endpoint/key
        // come from the canonical config via the runtime resolver.
        let embedder: Arc<dyn EmbeddingProvider> =
            Arc::from(super::embeddings::create_embedding_provider(
                model_provider,
                api_key,
                model,
                dimensions,
            ));
        self.swap_embedder(embedder);
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
        self.recall_scoped(query, limit, session_id, since, until, None)
            .await
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let scope = key_scope::PointScope::current();
        let conn = self.conn.clone();
        let key = key.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<MemoryEntry>> {
            let mut guard = conn.lock();
            let conn = guard.transaction()?;
            let Some(hit) = scope.select(key_scope::rows(&conn, &key, None)?, false).into_iter().next() else { return Ok(None) };
            let mut stmt = conn.prepare(
                "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, m.kind, m.pinned, a.alias, m.agent_id, m.tenant_id \
                 FROM memories m LEFT JOIN agents a ON a.id = m.agent_id \
                 WHERE m.id = ?1 AND (m.namespace IS NULL OR m.namespace != ?2)",
            )?;

            let mut rows = stmt.query_map(params![hit.id, crate::SOUL_NAMESPACE], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    key: key_scope::row_key(row)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    kind: Self::decode_kind(row.get(9)?),
                    pinned: row.get::<_, i64>(10)? != 0,
                    tenant_id: row.get(13)?,
                    agent_alias: row.get(11)?,
                    agent_id: row.get(12)?,
                })
            })?;

            match rows.next() {
                Some(Ok(entry)) => Ok(Some(entry)),
                Some(Err(error)) => Err(error.into()),
                None => Ok(None),
            }
        })
        .await?
    }

    async fn get_for_agent(
        &self,
        key: &str,
        agent_id: &str,
    ) -> anyhow::Result<Option<MemoryEntry>> {
        let scope = key_scope::PointScope::current();
        let conn = self.conn.clone();
        let key = key.to_string();
        let agent_id = agent_id.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<MemoryEntry>> {
            let mut guard = conn.lock();
            let conn = guard.transaction()?;
            let Some(hit) = scope.select(key_scope::rows(&conn, &key, Some(&agent_id))?, true).into_iter().next() else { return Ok(None) };
            let mut stmt = conn.prepare(
                "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, m.kind, m.pinned, a.alias, m.agent_id, m.tenant_id \
                 FROM memories m LEFT JOIN agents a ON a.id = m.agent_id \
                 WHERE m.id = ?1 AND m.agent_id = ?2",
            )?;

            let mut rows = stmt.query_map(params![hit.id, agent_id], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    key: key_scope::row_key(row)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    kind: Self::decode_kind(row.get(9)?),
                    pinned: row.get::<_, i64>(10)? != 0,
                    tenant_id: row.get(13)?,
                    agent_alias: row.get(11)?,
                    agent_id: row.get(12)?,
                })
            })?;

            match rows.next() {
                Some(Ok(entry)) => Ok(Some(entry)),
                Some(Err(error)) => Err(error.into()),
                None => Ok(None),
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
                    key: key_scope::row_key(row)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    kind: Self::decode_kind(row.get(9)?),
                    pinned: row.get::<_, i64>(10)? != 0,
                    tenant_id: row.get(13)?,
                    agent_alias: row.get(11)?,
                    agent_id: row.get(12)?,
                })
            };

            if let Some(ref cat) = category {
                let cat_str = Self::category_to_str(cat);
                let mut stmt = conn.prepare(
                    "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, m.kind, m.pinned, a.alias, m.agent_id, m.tenant_id
                     FROM memories m LEFT JOIN agents a ON a.id = m.agent_id
                     WHERE m.superseded_by IS NULL AND m.category = ?1 AND (m.namespace IS NULL OR m.namespace != ?3) ORDER BY m.updated_at DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![cat_str, DEFAULT_LIST_LIMIT, crate::SOUL_NAMESPACE], row_mapper)?;
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
                    "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, m.kind, m.pinned, a.alias, m.agent_id, m.tenant_id
                     FROM memories m LEFT JOIN agents a ON a.id = m.agent_id
                     WHERE m.superseded_by IS NULL AND (m.namespace IS NULL OR m.namespace != ?2) ORDER BY m.updated_at DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![DEFAULT_LIST_LIMIT, crate::SOUL_NAMESPACE], row_mapper)?;
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
        self.forget_key(key, None).await
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> anyhow::Result<bool> {
        self.forget_key(key, Some(agent_id)).await
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
            let affected = conn.execute(
                "DELETE FROM memories WHERE agent_id = (SELECT id FROM agents WHERE alias = ?1 LIMIT 1)",
                params![agent_alias],
            )?;
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            Ok(affected)
        })
        .await?
    }

    async fn archive_and_purge_agent(
        &self,
        agent_alias: &str,
        archive_path: &std::path::Path,
    ) -> anyhow::Result<usize> {
        let conn = self.conn.clone();
        let agent_alias = agent_alias.to_string();
        let archive_path = archive_path.to_path_buf();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            use zeroclaw_infra::owned_archive::{sqlite_rows, write_verified_json};
            let mut conn = conn.lock();
            let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let agents = sqlite_rows(&transaction,
                "SELECT * FROM agents WHERE alias = ?1 ORDER BY id", &agent_alias)?;
            let memories = sqlite_rows(&transaction,
                "SELECT * FROM memories WHERE agent_id = (SELECT id FROM agents WHERE alias = ?1 LIMIT 1) ORDER BY id", &agent_alias)?;
            // The typed export is deliberately not used here: it omits stored
            // embeddings, update timestamps and future schema columns.
            let snapshot = serde_json::json!({
                "format": "zeroclaw-agent-archive", "version": 1, "store": "sqlite-memory", "agent_alias": agent_alias,
                "tables": {"agents": agents, "memories": memories},
            });
            write_verified_json(&archive_path, &snapshot)?;
            let removed = transaction.execute(
                "DELETE FROM memories WHERE agent_id = (SELECT id FROM agents WHERE alias = ?1 LIMIT 1)",
                [&agent_alias],
            )?;
            transaction.commit()?;
            Ok(removed)
        }).await?
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
        if self.embedder.read().dimensions() == 0 {
            return Ok(0);
        }

        let conn = self.conn.clone();
        let entries: Vec<(String, String)> = tokio::task::spawn_blocking(move || {
            let conn = conn.lock();
            let mut stmt = conn.prepare(
                "SELECT id, content FROM memories WHERE embedding IS NULL \
                 AND (namespace IS NULL OR namespace != ?1)",
            )?;
            let rows = stmt.query_map(params![crate::SOUL_NAMESPACE], |row| {
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
                "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, m.kind, m.pinned, a.alias, m.agent_id, m.tenant_id \
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
                    key: key_scope::row_key(row)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    kind: Self::decode_kind(row.get(9)?),
                    pinned: row.get::<_, i64>(10)? != 0,
                    tenant_id: row.get(13)?,
                    agent_alias: row.get(11)?,
                    agent_id: row.get(12)?,
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
                "SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, m.namespace, m.importance, m.superseded_by, m.kind, m.pinned, a.alias, m.agent_id, m.tenant_id \
                 FROM memories m LEFT JOIN agents a ON a.id = m.agent_id \
                 WHERE m.agent_id = (SELECT id FROM agents WHERE alias = ?1 LIMIT 1) \
                 ORDER BY m.created_at ASC",
            )?;
            let rows = stmt.query_map(params![agent_alias], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    key: key_scope::row_key(row)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                    namespace: row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "default".into()),
                    importance: row.get(7)?,
                    superseded_by: row.get(8)?,
                    kind: Self::decode_kind(row.get(9)?),
                    pinned: row.get::<_, i64>(10)? != 0,
                    tenant_id: row.get(13)?,
                    agent_alias: row.get(11)?,
                    agent_id: row.get(12)?,
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
        // The namespace restriction is pushed into every search channel
        // (FTS, vector, time-only) rather than post-filtering an ambient
        // recall, because ambient recall structurally excludes the
        // reserved Soul namespace; a post-filter on top of it could never
        // recover namespaced rows.
        self.recall_scoped_with_namespace(
            query,
            limit,
            session_id,
            since,
            until,
            None,
            Some(namespace),
        )
        .await
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
        self.store_row_with_metadata(
            key,
            content,
            category,
            session_id,
            StoreOptions {
                namespace: namespace.map(str::to_string),
                importance,
                ..StoreOptions::default()
            },
            None,
        )
        .await
    }

    async fn store_with_options(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        options: StoreOptions,
    ) -> anyhow::Result<()> {
        self.store_row_with_metadata(key, content, category, session_id, options, None)
            .await
    }

    async fn store_with_options_and_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        options: StoreOptions,
        agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.store_row_with_metadata(key, content, category, session_id, options, agent_id)
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
        self.store_row_with_metadata(
            key,
            content,
            category,
            session_id,
            StoreOptions {
                namespace: namespace.map(str::to_string),
                importance,
                ..StoreOptions::default()
            },
            agent_id,
        )
        .await
    }

    async fn supersede(&self, superseded_ids: &[String], new_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.clone();
        let ids = superseded_ids.to_vec();
        let new_id = new_id.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = conn.lock();
            crate::conflict::mark_superseded(&conn, &ids, &new_id)
        })
        .await?
    }

    async fn count_in_scope(
        &self,
        namespace: Option<&str>,
        category: Option<&MemoryCategory>,
    ) -> anyhow::Result<u64> {
        let conn = self.conn.clone();
        let namespace = namespace.map(str::to_string);
        let category = category.map(Self::category_to_str);
        tokio::task::spawn_blocking(move || -> anyhow::Result<u64> {
            let conn = conn.lock();
            let count = match (namespace, category) {
                (Some(ns), Some(cat)) => conn.query_row(
                    "SELECT COUNT(*) FROM memories WHERE namespace = ?1 AND category = ?2 AND superseded_by IS NULL",
                    params![ns, cat],
                    |row| row.get::<_, u64>(0),
                )?,
                (Some(ns), None) => conn.query_row(
                    "SELECT COUNT(*) FROM memories WHERE namespace = ?1 AND superseded_by IS NULL",
                    params![ns],
                    |row| row.get::<_, u64>(0),
                )?,
                (None, Some(cat)) => conn.query_row(
                    "SELECT COUNT(*) FROM memories WHERE category = ?1 AND superseded_by IS NULL",
                    params![cat],
                    |row| row.get::<_, u64>(0),
                )?,
                (None, None) => conn.query_row(
                    "SELECT COUNT(*) FROM memories WHERE superseded_by IS NULL",
                    [],
                    |row| row.get::<_, u64>(0),
                )?,
            };
            Ok(count)
        })
        .await?
    }

    async fn stats(&self) -> anyhow::Result<MemoryStats> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<MemoryStats> {
            let conn = conn.lock();
            let total_rows = conn.query_row("SELECT COUNT(*) FROM memories", [], |row| {
                row.get::<_, u64>(0)
            })?;
            let superseded_rows = conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE superseded_by IS NOT NULL",
                [],
                |row| row.get::<_, u64>(0),
            )?;
            let pinned_rows = conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE pinned = 1",
                [],
                |row| row.get::<_, u64>(0),
            )?;
            let bytes = conn.query_row(
                "SELECT COALESCE(SUM(LENGTH(content)), 0) FROM memories",
                [],
                |row| row.get::<_, u64>(0),
            )?;
            let mut stmt =
                conn.prepare("SELECT category, COUNT(*) FROM memories GROUP BY category")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?;
            let by_category = rows.collect::<Result<Vec<_>, _>>()?;
            Ok(MemoryStats {
                total_rows,
                by_category,
                superseded_rows,
                pinned_rows,
                bytes,
            })
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
        if allowed_agent_ids.is_empty() {
            return self.recall(query, limit, session_id, since, until).await;
        }

        let allowed: Vec<String> = allowed_agent_ids.iter().map(|s| (*s).to_string()).collect();
        self.recall_scoped(query, limit, session_id, since, until, Some(allowed))
            .await
    }

    async fn ensure_agent_uuid(&self, alias: &str) -> anyhow::Result<String> {
        let conn = self.conn.clone();
        let alias = alias.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let conn = conn.lock();
            crate::migration::sqlite_ensure_agent_uuid(&conn, &alias)
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
mod tests;
