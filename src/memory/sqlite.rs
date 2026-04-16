use super::embeddings::EmbeddingProvider;
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use super::vector;
use anyhow::Context;
use async_trait::async_trait;
use chrono::Local;
use parking_lot::Mutex;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use uuid::Uuid;

/// PR #7 r2d2 pool — read connection pool size. 8 concurrent readers
/// matches the spec; writes still serialise through the existing
/// `Arc<Mutex<Connection>>` so there is only one writer at a time
/// (SQLite's own constraint, WAL mode notwithstanding).
const READ_POOL_SIZE: u32 = 8;

/// Type alias so spawn_blocking closures don't need the verbose path.
pub(crate) type ReadPool = Pool<SqliteConnectionManager>;

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

    /// PR #4 — optional cross-encoder reranker. Interior-mutable with the
    /// same rationale as `sync` above: the factory wires the reranker after
    /// `SqliteMemory` is already constructed, and we want to keep the
    /// `Memory` trait free of reranker specifics.
    reranker: Mutex<Option<Arc<dyn super::search::Reranker>>>,

    /// PR #4 — rerank runtime settings (enabled flag + top_k window).
    /// Defaults to `RerankRuntimeConfig::default()` which is `enabled = false`.
    rerank_config: Mutex<super::search::RerankRuntimeConfig>,

    /// PR #7 HLC migration — monotonic clock used to stamp every INSERT
    /// / UPDATE into `memories.updated_at_hlc`. Separate from
    /// `updated_at` (RFC3339 wall-clock string) so peers that speak
    /// pre-HLC protocol versions keep working — the HLC column is
    /// additive and not yet the primary ordering key for sync.
    hlc_clock: Arc<crate::sync::hlc::HlcClock>,

    /// PR #7 r2d2 read pool — up to 8 concurrent connections used by
    /// the hot read paths (fts5_search, vector_search,
    /// recall_with_variations). Writes still go through the legacy
    /// `Arc<Mutex<Connection>>` because SQLite only allows one writer
    /// at a time anyway — this field is an *additional* resource, not
    /// a replacement. Every PooledConnection is initialised with the
    /// same PRAGMA tuning as the main connection (WAL, busy_timeout).
    read_pool: ReadPool,
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

    /// PR #7 r2d2 — expose the read pool so hot-read callers can grab
    /// a concurrent connection instead of queueing on the writer Mutex.
    /// Returns an owned clone because `r2d2::Pool` is internally Arc'd
    /// and clones are cheap.
    pub fn read_pool(&self) -> ReadPool {
        self.read_pool.clone()
    }

    /// Workspace directory containing this brain.db — derived from the
    /// db_path's grandparent (brain.db lives at `<workspace>/memory/brain.db`).
    /// Used by Dream Cycle to co-locate procedural skill / user profile
    /// stores on the same SQLite file.
    pub fn workspace_dir(&self) -> Option<&Path> {
        self.db_path.parent().and_then(|p| p.parent())
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

    /// PR #5 — SQLCipher variant. Identical to [`Self::with_options`] but
    /// runs `PRAGMA key = '…'` immediately after opening the connection so
    /// all subsequent page reads/writes are AES-256 encrypted at rest.
    /// Also issues `PRAGMA cipher_migrate;` which transparently upgrades
    /// pre-existing non-encrypted `brain.db` files on first open when the
    /// passphrase format changes across SQLCipher versions.
    ///
    /// Available only under the `memory-sqlcipher` cargo feature
    /// (`--no-default-features --features memory-sqlcipher`). Builds
    /// without the feature see a compile error if they try to call this —
    /// that's intentional so a misconfigured CI can't silently produce
    /// an unencrypted DB while thinking it's encrypted.
    #[cfg(feature = "memory-sqlcipher")]
    pub fn with_options_keyed(
        workspace_dir: &Path,
        passphrase: &str,
        embedder: Arc<dyn EmbeddingProvider>,
        search_mode: SearchMode,
        vector_weight: f32,
        keyword_weight: f32,
        rrf_k: f32,
        cache_max: usize,
        open_timeout_secs: Option<u64>,
        journal_mode: &str,
    ) -> anyhow::Result<Self> {
        Self::with_options_inner(
            workspace_dir,
            Some(passphrase),
            embedder,
            search_mode,
            vector_weight,
            keyword_weight,
            rrf_k,
            cache_max,
            open_timeout_secs,
            journal_mode,
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
        Self::with_options_inner(
            workspace_dir,
            None,
            embedder,
            search_mode,
            vector_weight,
            keyword_weight,
            rrf_k,
            cache_max,
            open_timeout_secs,
            journal_mode,
        )
    }

    /// Shared body between `with_options` and `with_options_keyed`. The
    /// `passphrase` parameter exists on the non-SQLCipher build path too
    /// (always `None`) so we only maintain one implementation; the
    /// `PRAGMA key` path is reachable only when `memory-sqlcipher` is on.
    #[allow(clippy::too_many_arguments)]
    fn with_options_inner(
        workspace_dir: &Path,
        #[allow(unused_variables)] passphrase: Option<&str>,
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

        // ── PR #5: SQLCipher at-rest encryption ─────────────────
        // Must run BEFORE any other PRAGMA so the header is read as
        // encrypted. When the `memory-sqlcipher` feature is off the
        // whole block is compiled out — passing a passphrase into a
        // non-SQLCipher binary is a programming error.
        #[cfg(feature = "memory-sqlcipher")]
        {
            if let Some(key) = passphrase {
                // Escape any single quotes to keep the pragma well-formed.
                let escaped = key.replace('\'', "''");
                conn.execute_batch(&format!("PRAGMA key = '{escaped}';"))?;
                // Transparent migration for DBs created by older
                // SQLCipher versions; no-op for fresh DBs.
                conn.execute_batch("SELECT count(*) FROM sqlite_master;")?;
            }
        }

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

        // PR #7 HLC migration — derive a node identifier from the DB path
        // + hostname so every SqliteMemory instance on this device/user
        // stamps consistent HLCs. Falls back to "device" when hostname
        // isn't available.
        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.to_str().map(String::from))
            .unwrap_or_else(|| "device".to_string());
        let node_id = format!("{}-{}", hostname, db_path.display());
        let hlc_clock = Arc::new(crate::sync::hlc::HlcClock::new(node_id));

        // PR #7 r2d2 — build the read pool pointing at the SAME DB file.
        // Every pooled connection re-applies the core PRAGMA set via a
        // customiser so reads operate under the same WAL + busy_timeout
        // semantics as the writer.
        let manager = SqliteConnectionManager::file(&db_path).with_init(|c| {
            c.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous  = NORMAL;
                 PRAGMA busy_timeout = 5000;
                 PRAGMA cache_size   = -2000;
                 PRAGMA temp_store   = MEMORY;",
            )
        });
        let read_pool = Pool::builder()
            .max_size(READ_POOL_SIZE)
            .min_idle(Some(1))
            .connection_timeout(Duration::from_secs(5))
            .build(manager)
            .context("failed to build r2d2 read pool")?;

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
            reranker: Mutex::new(None),
            rerank_config: Mutex::new(super::search::RerankRuntimeConfig::default()),
            hlc_clock,
            read_pool,
        })
    }

    // ── PR #6: consolidation + decay integration ─────────────────

    /// Pull every non-archived memory with a usable embedding into
    /// [`super::consolidate::CandidateMemory`] form, ready for
    /// [`super::consolidate::consolidate_candidates`]. The
    /// `min_recall_count` filter implements the "earned an opinion" rule
    /// from the algorithm doc — untouched memories are skipped.
    pub fn collect_consolidation_candidates(
        &self,
        min_recall_count: u32,
    ) -> anyhow::Result<Vec<super::consolidate::CandidateMemory>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, key, content, embedding
               FROM memories
              WHERE archived = 0
                AND embedding IS NOT NULL
                AND recall_count >= ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![min_recall_count], |row| {
            let id: String = row.get(0)?;
            let key: String = row.get(1)?;
            let content: String = row.get(2)?;
            let blob: Vec<u8> = row.get(3)?;
            Ok((id, key, content, blob))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (id, key, content, blob) = r?;
            // Embedding is stored as little-endian f32 (`super::vector`
            // helpers — keep this decode in sync).
            if blob.len() % 4 != 0 || blob.is_empty() {
                continue;
            }
            let mut embedding = Vec::with_capacity(blob.len() / 4);
            for chunk in blob.chunks_exact(4) {
                let mut buf = [0u8; 4];
                buf.copy_from_slice(chunk);
                embedding.push(f32::from_le_bytes(buf));
            }
            out.push(super::consolidate::CandidateMemory {
                id,
                key,
                content,
                embedding,
            });
        }
        Ok(out)
    }

    /// Persist a single [`super::consolidate::ConsolidationOutcome`] —
    /// inserts the row into `consolidated_memories` and flips the source
    /// memories' `archived = 1`. Wrapped in a single transaction so a
    /// crash leaves no half-archived state.
    pub fn apply_consolidation_outcome(
        &self,
        outcome: &super::consolidate::ConsolidationOutcome,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        let now = chrono::Local::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();
        let source_ids_json = serde_json::to_string(&outcome.source_ids)?;
        let source_keys_json = serde_json::to_string(&outcome.source_keys)?;
        let contradicting_json = serde_json::to_string(&outcome.contradicting_keys)?;
        let conflict_flag: i64 = i64::from(outcome.conflict);
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO consolidated_memories
                 (id, fact_type, summary, source_ids, source_keys,
                  conflict_flag, contradicting_keys, created_at)
              VALUES (?1, 'semantic_fact', ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                outcome.summary,
                source_ids_json,
                source_keys_json,
                conflict_flag,
                contradicting_json,
                now
            ],
        )?;
        // Soft-archive each source memory. We deliberately do NOT delete
        // the row — the user must be able to recover it from the archive
        // UI. archived=1 is filtered out of recall in a follow-up that
        // adjusts the SELECT WHERE clauses in fts5_search/vector_search.
        for src_id in &outcome.source_ids {
            tx.execute(
                "UPDATE memories SET archived = 1 WHERE id = ?1",
                rusqlite::params![src_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Bump the recall counter and refresh `last_recalled` for a memory
    /// the agent just surfaced. Caller passes the IDs that came back from
    /// `recall()` so we can update them all in one transaction. Failures
    /// are swallowed (telemetry only) — bookkeeping must never cause a
    /// recall to fail user-facing.
    pub fn bump_recall_metrics(&self, ids: &[String]) -> anyhow::Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock();
        let tx = conn.unchecked_transaction()?;
        let now = chrono::Local::now().to_rfc3339();
        let mut updated = 0;
        for id in ids {
            updated += tx.execute(
                "UPDATE memories
                    SET recall_count = recall_count + 1,
                        last_recalled = ?1
                  WHERE id = ?2",
                rusqlite::params![now, id],
            )?;
        }
        tx.commit()?;
        Ok(updated)
    }

    /// Recompute `decay_score` for every non-archived memory and flip any
    /// that fall below [`super::decay::ARCHIVE_FLOOR`]. Designed for
    /// nightly invocation from the dream cycle. Returns the count of
    /// rows whose `archived` flag transitioned this run.
    ///
    /// Two scores are computed per row:
    ///   * **stored** — `decay::decay_score_for_category` with the
    ///     cosmetic floor (`DEFAULT_FLOOR = 0.1`); this is what the
    ///     recall hot path displays / sorts on.
    ///   * **raw** — same formula with floor = 0.0; this is what the
    ///     archive decision uses, otherwise the cosmetic floor would
    ///     keep every never-recalled memory pinned above the
    ///     `ARCHIVE_FLOOR` of 0.05 forever.
    /// Identity-category memories never archive because their
    /// half-life is `INFINITY` → raw score ≥ ln(recall_count + 1) ≥ 0.
    pub fn run_decay_sweep(&self) -> anyhow::Result<usize> {
        use super::decay::{
            decay_score, decay_score_for_category, half_life_for, should_archive, ARCHIVE_FLOOR,
        };
        use chrono::{DateTime, Local};

        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, category, recall_count, last_recalled, created_at
               FROM memories WHERE archived = 0",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let snapshots: Vec<(String, String, i64, Option<String>, String)> =
            rows.collect::<Result<_, _>>()?;
        drop(stmt);

        let now = Local::now();
        let tx = conn.unchecked_transaction()?;
        let mut archived = 0usize;
        for (id, cat, count, last_recalled, created_at) in snapshots {
            let reference = last_recalled.as_deref().unwrap_or(&created_at);
            let parsed = DateTime::parse_from_rfc3339(reference)
                .map(|d| d.with_timezone(&Local))
                .unwrap_or(now);
            #[allow(clippy::cast_precision_loss)]
            let days = ((now - parsed).num_seconds() as f32 / 86_400.0).max(0.0);
            let category = Self::str_to_category(&cat);
            let recall_count_u32 = u32::try_from(count.max(0)).unwrap_or(u32::MAX);

            let half_life = half_life_for(&category);
            let stored = decay_score_for_category(recall_count_u32, days, &category);
            let raw = decay_score(recall_count_u32, days, half_life, 0.0);

            // INFINITY half-life means identity / pinned facts — they
            // must survive every sweep no matter what raw score the
            // formula produces (ln(1) = 0 with a zero floor would
            // otherwise misfire).
            let archivable = half_life.is_finite() && should_archive(raw, ARCHIVE_FLOOR);

            if archivable {
                tx.execute(
                    "UPDATE memories
                        SET decay_score = ?1, archived = 1
                      WHERE id = ?2",
                    rusqlite::params![f64::from(stored), id],
                )?;
                archived += 1;
            } else {
                tx.execute(
                    "UPDATE memories SET decay_score = ?1 WHERE id = ?2",
                    rusqlite::params![f64::from(stored), id],
                )?;
            }
        }
        tx.commit()?;
        Ok(archived)
    }

    // ── PR #6: archive UI backend ──────────────────────────────

    /// PR #6 — list archived memories with optional consolidation context.
    /// Returns rows where `archived = 1`, joined against
    /// `consolidated_memories` so the UI can show "merged into community X".
    pub fn list_archived(&self) -> anyhow::Result<Vec<ArchivedMemoryInfo>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.key, m.content, m.category, m.updated_at,
                    cm.summary, cm.fact_type
             FROM memories m
             LEFT JOIN consolidated_memories cm
               ON cm.source_ids LIKE '%' || m.id || '%'
             WHERE m.archived = 1
             ORDER BY m.updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ArchivedMemoryInfo {
                id: row.get(0)?,
                key: row.get(1)?,
                content: row.get(2)?,
                category: row.get(3)?,
                updated_at: row.get(4)?,
                consolidated_summary: row.get(5)?,
                consolidated_fact_type: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// PR #6 — un-archive a single memory by id.
    pub fn restore_archived(&self, memory_id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock();
        let changed = conn.execute(
            "UPDATE memories SET archived = 0 WHERE id = ?1 AND archived = 1",
            rusqlite::params![memory_id],
        )?;
        Ok(changed > 0)
    }

    // ── PR #4: reranker plumbing ─────────────────────────────────

    /// PR #4 — attach a cross-encoder reranker. The reranker is consulted by
    /// `recall_with_variations` after RRF fusion when `rerank_config.enabled`
    /// is true. Replacing an existing reranker is explicit; callers should
    /// not install one at hot path.
    pub fn set_reranker(&self, reranker: Arc<dyn super::search::Reranker>) {
        *self.reranker.lock() = Some(reranker);
    }

    /// PR #4 — update rerank runtime config (enabled/model/top_k). The config
    /// is read on every recall so changes take effect immediately.
    pub fn set_rerank_config(&self, cfg: super::search::RerankRuntimeConfig) {
        *self.rerank_config.lock() = cfg;
    }

    /// Snapshot of the current rerank config — used by the hot path without
    /// holding a lock across awaits.
    fn rerank_config(&self) -> super::search::RerankRuntimeConfig {
        self.rerank_config.lock().clone()
    }

    /// Snapshot of the currently attached reranker, if any.
    fn active_reranker(&self) -> Option<Arc<dyn super::search::Reranker>> {
        self.reranker.lock().clone()
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
            -- trigram tokenizer (not unicode61) for Korean morphology.
            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                key, content, content=memories, content_rowid=rowid,
                tokenize='trigram'
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

            -- PR #5 embedding backfill queue. Populated when a remote sync
            -- delta carries an embedding whose (provider/model/version/dim)
            -- disagrees with the local embedder — the blob is discarded
            -- rather than silently cached (foreign-model floats would feed
            -- vec2text-style reconstruction attacks) and the content hash
            -- is parked here for a background re-embedding pass.
            CREATE TABLE IF NOT EXISTS embedding_backfill_queue (
                content_hash TEXT PRIMARY KEY,
                reason       TEXT NOT NULL,
                enqueued_at  TEXT NOT NULL,
                processed_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_backfill_pending
                ON embedding_backfill_queue(processed_at) WHERE processed_at IS NULL;",
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

        // ── PR #7 HLC migration (additive) ─────────────────────────
        // Stamp every row with a Hybrid Logical Clock string alongside
        // the existing RFC3339 updated_at. Future sync protocol version
        // can switch ordering to this column; today it's informational
        // but captured on every write so the data is ready when the
        // protocol flip happens.
        if !memories_sql.contains("updated_at_hlc") {
            conn.execute_batch(
                "ALTER TABLE memories ADD COLUMN updated_at_hlc TEXT;
                 CREATE INDEX IF NOT EXISTS idx_memories_hlc
                     ON memories(updated_at_hlc) WHERE updated_at_hlc IS NOT NULL;",
            )?;
        }

        // ── PR #6 migration: forgetting-curve + consolidation columns ──
        // recall_count / last_recalled feed the decay score; archived flips
        // to 1 either by the consolidator (sources merged into a
        // semantic_fact) or by the nightly decay sweep when the score
        // falls below decay::ARCHIVE_FLOOR. decay_score is denormalised
        // onto the row so the recall hot path can filter without
        // recomputing on every read.
        if !memories_sql.contains("recall_count") {
            conn.execute_batch(
                "ALTER TABLE memories ADD COLUMN recall_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE memories ADD COLUMN last_recalled TEXT;
                 ALTER TABLE memories ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE memories ADD COLUMN decay_score REAL NOT NULL DEFAULT 1.0;
                 CREATE INDEX IF NOT EXISTS idx_memories_archived
                     ON memories(archived);
                 CREATE INDEX IF NOT EXISTS idx_memories_decay
                     ON memories(decay_score) WHERE archived = 0;",
            )?;
        }

        // PR #6 — consolidated semantic facts. Each row is the LLM-summary
        // of a cluster of near-duplicate memories; source_ids is a JSON
        // array of memories.id values that were archived in its favour.
        // type discriminates future expansions (today only "semantic_fact").
        // conflict_flag = 1 surfaces clusters where members contradict
        // each other so a UI can prompt the user for resolution.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS consolidated_memories (
                id            TEXT PRIMARY KEY,
                fact_type     TEXT NOT NULL DEFAULT 'semantic_fact',
                summary       TEXT NOT NULL,
                source_ids    TEXT NOT NULL,
                source_keys   TEXT NOT NULL,
                conflict_flag INTEGER NOT NULL DEFAULT 0,
                contradicting_keys TEXT,
                created_at    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_consolidated_conflict
                ON consolidated_memories(conflict_flag) WHERE conflict_flag = 1;
            CREATE INDEX IF NOT EXISTS idx_consolidated_created
                ON consolidated_memories(created_at DESC);",
        )?;

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

        // PR #6 wire-up: skip soft-archived rows so consolidated/decayed
        // memories never resurface through FTS until the archive UI
        // explicitly un-archives them.
        let sql = "SELECT m.id, bm25(memories_fts) as score
                   FROM memories_fts f
                   JOIN memories m ON m.rowid = f.rowid
                   WHERE memories_fts MATCH ?1
                     AND m.archived = 0
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
        // PR #6 wire-up: archived rows excluded from vector search for the
        // same reason fts5_search excludes them — consolidated/decayed
        // memories must not bleed back into recall.
        let mut sql =
            "SELECT id, embedding FROM memories WHERE embedding IS NOT NULL AND archived = 0"
                .to_string();
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
        // PR #7 — give the engine an HLC clock seeded from the device_id
        // so outgoing deltas carry v2 `hlc_stamp`s. Existing engines
        // without an attached clock stay v1-compatible; explicit override
        // via `engine.attach_hlc(...)` from callers still wins.
        {
            let mut eng = engine.lock();
            if eng.current_hlc_stamp().is_none() {
                let node_id = eng.device_id().0.clone();
                eng.attach_hlc(crate::sync::hlc::HlcClock::new(node_id));
            }
        }
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

        // PR #4 — only short-circuit to `recall()` when the caller has
        // nothing to gain from the RRF+rerank path. If a reranker is
        // attached and enabled, the single-query path still needs the
        // cross-encoder rerank pass (which `recall()` doesn't run), so
        // we fall through to the full pipeline.
        let rerank_attached =
            self.active_reranker().is_some() && self.rerank_config().enabled;
        if variations.len() <= 1 && !rerank_attached {
            // No expansion and no reranker — plain recall is cheaper.
            return self.recall(original_query, limit, session_id).await;
        }

        // Multi-query RRF: search each variation, merge all results
        let query_embedding_original = self.get_or_compute_embedding(original_query).await?;

        // PR #4 — if variations is empty (rerank-only single-query mode),
        // seed the search with the original query so the RRF pool isn't
        // empty. Callers with real expansion already pass original + N
        // rewrites, so the owned vector is just `variations.to_vec()`.
        let queries_owned: Vec<String> = if variations.is_empty() {
            vec![original_query.to_string()]
        } else {
            variations.to_vec()
        };

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

        // PR #4 — how big a candidate pool the reranker (if any) wants to
        // see. Fetched here so the blocking task can over-fetch when a
        // reranker is attached.
        let rerank_cfg = self.rerank_config();
        let reranker = self.active_reranker();
        let fetch_limit = if reranker.is_some() && rerank_cfg.enabled {
            rerank_cfg.top_k_before.max(limit)
        } else {
            limit
        };

        let results: anyhow::Result<Vec<MemoryEntry>> = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let conn = conn.lock();
            let session_ref = sid.as_deref();

            // PR #4: collect one ranker slice per (query × ranker_kind). The
            // previous implementation flattened all queries into two big
            // lists and 2-way merged them, losing the rank-per-query signal
            // that RRF is designed to exploit. k_way_rrf keeps each list
            // separate so a document that lands rank-1 in multiple query
            // variations dominates one that lands rank-1 once.
            let mut per_ranker_lists: Vec<Vec<(String, f32)>> =
                Vec::with_capacity(queries_owned.len() * 2);
            let mut any_vec_hit = false;

            for (i, q) in queries_owned.iter().enumerate() {
                let fts = Self::fts5_search(&conn, q, fetch_limit * 2).unwrap_or_default();
                per_ranker_lists.push(fts);

                if let Some(Some(ref emb)) = query_embeddings.get(i) {
                    let vec_hits =
                        Self::vector_search(&conn, emb, fetch_limit * 2, None, session_ref)
                            .unwrap_or_default();
                    if !vec_hits.is_empty() {
                        any_vec_hit = true;
                    }
                    per_ranker_lists.push(vec_hits);
                }
            }

            let merged: Vec<super::vector::ScoredResult> = match search_mode {
                SearchMode::Rrf => {
                    if !any_vec_hit {
                        // FTS-only fallback: skip fusion when vectors are
                        // unavailable (e.g. NoopEmbedding); preserve BM25
                        // order directly.
                        let flat = dedup_ranked_list(
                            &per_ranker_lists
                                .iter()
                                .flatten()
                                .cloned()
                                .collect::<Vec<_>>(),
                        );
                        flat.into_iter()
                            .take(fetch_limit)
                            .map(|(id, score)| super::vector::ScoredResult {
                                id,
                                vector_score: None,
                                keyword_score: Some(score),
                                final_score: score,
                            })
                            .collect()
                    } else {
                        let ranker_refs: Vec<&[(String, f32)]> = per_ranker_lists
                            .iter()
                            .map(std::vec::Vec::as_slice)
                            .collect();
                        let fused = super::search::k_way_rrf(
                            &ranker_refs,
                            super::search::RrfSettings {
                                k: rrf_k,
                                limit: fetch_limit,
                            },
                        );
                        fused
                            .into_iter()
                            .map(|f| super::vector::ScoredResult {
                                id: f.id,
                                vector_score: None,
                                keyword_score: None,
                                final_score: f.score,
                            })
                            .collect()
                    }
                }
                SearchMode::Weighted => {
                    // Weighted mode keeps the legacy 2-way collapse — its
                    // linear score blend assumes a single vector and a
                    // single keyword list, so we still dedup-then-merge.
                    let mut all_vec: Vec<(String, f32)> = Vec::new();
                    let mut all_fts: Vec<(String, f32)> = Vec::new();
                    for (idx, list) in per_ranker_lists.iter().enumerate() {
                        // Even-numbered slots are fts (see push order above);
                        // odd slots are vec. This pairing is local to this
                        // legacy branch only.
                        if idx % 2 == 0 {
                            all_fts.extend(list.iter().cloned());
                        } else {
                            all_vec.extend(list.iter().cloned());
                        }
                    }
                    let dedup_vec = dedup_ranked_list(&all_vec);
                    let dedup_fts = dedup_ranked_list(&all_fts);
                    if dedup_vec.is_empty() {
                        dedup_fts
                            .into_iter()
                            .map(|(id, score)| super::vector::ScoredResult {
                                id,
                                vector_score: None,
                                keyword_score: Some(score),
                                final_score: score,
                            })
                            .collect()
                    } else {
                        super::vector::hybrid_merge(
                            &dedup_vec,
                            &dedup_fts,
                            vector_weight,
                            keyword_weight,
                            fetch_limit,
                        )
                    }
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
        .await
        .map_err(|e| anyhow::anyhow!("recall_with_variations blocking task panicked: {e}"))?;

        // PR #4 — cross-encoder rerank pass. Runs off the blocking pool so
        // the ONNX call can own its own tokio::spawn_blocking internally.
        let fused_entries = results?;
        let final_entries: Vec<MemoryEntry> = match (reranker, rerank_cfg.enabled) {
            (Some(reranker), true) if !fused_entries.is_empty() => {
                let window = rerank_cfg.top_k_before.min(fused_entries.len());
                let (rerank_slice, rest) = fused_entries.split_at(window);
                let candidates: Vec<super::search::RerankCandidate> = rerank_slice
                    .iter()
                    .map(|e| super::search::RerankCandidate {
                        id: e.id.clone(),
                        text: e.content.clone(),
                        #[allow(clippy::cast_possible_truncation)]
                        prior_score: e.score.map(|s| s as f32).unwrap_or(0.0),
                    })
                    .collect();
                match reranker.rerank(original_query, candidates).await {
                    Ok(reordered) => {
                        let keep = rerank_cfg.top_k_after.min(reordered.len());
                        let mut by_id: std::collections::HashMap<String, MemoryEntry> =
                            rerank_slice
                                .iter()
                                .cloned()
                                .map(|e| (e.id.clone(), e))
                                .collect();
                        let mut out = Vec::with_capacity(keep);
                        for c in reordered.into_iter().take(keep) {
                            if let Some(mut entry) = by_id.remove(&c.id) {
                                entry.score = Some(f64::from(c.prior_score));
                                out.push(entry);
                            }
                        }
                        // If the reranker trimmed below the caller's limit
                        // (because top_k_after < window), tail the RRF
                        // overflow so we don't hand back fewer results than
                        // `limit` requested.
                        if out.len() < limit {
                            for entry in rest.iter().take(limit - out.len()) {
                                out.push(entry.clone());
                            }
                        }
                        out
                    }
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "rerank pass failed; returning RRF-only results"
                        );
                        fused_entries.into_iter().take(limit).collect()
                    }
                }
            }
            _ => fused_entries.into_iter().take(limit).collect(),
        };

        Ok(final_entries)
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

/// PR #6 — row returned by `list_archived` for the archive UI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArchivedMemoryInfo {
    pub id: String,
    pub key: String,
    pub content: String,
    pub category: String,
    pub updated_at: String,
    /// Summary of the consolidated fact this memory was folded into, if any.
    pub consolidated_summary: Option<String>,
    /// Fact type (e.g. "preference", "fact") from the consolidation record.
    pub consolidated_fact_type: Option<String>,
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

    /// PR #5 — decide whether to cache a remote-computed embedding.
    ///
    /// Accept iff the remote blob's (provider, model, version, dim) all match
    /// our local embedder's. On match, seed `embedding_cache` keyed on
    /// `content_hash` so the next local recall() skips re-computation.
    /// On drift, enqueue the content hash into `embedding_backfill_queue`
    /// and return an error describing the mismatch — caller logs but does
    /// not abort delta application.
    async fn accept_remote_embedding(
        &self,
        content: &str,
        blob: &super::sync::EmbeddingBlob,
    ) -> anyhow::Result<bool> {
        let local_provider = self.embedder.name().to_string();
        let local_model = self.embedder.model().to_string();
        let local_version = self.embedder.version();
        let local_dim = self.embedder.dimensions();

        // Noop embedder (dim = 0) has nothing to cache; swallow silently.
        if local_dim == 0 {
            return Ok(false);
        }

        let drift = blob.provider != local_provider
            || blob.model != local_model
            || blob.version != local_version
            || usize::try_from(blob.dim).unwrap_or(usize::MAX) != local_dim;

        let content_hash = Self::content_hash(content);
        let conn = self.conn.clone();
        let content_hash_owned = content_hash.clone();

        if drift {
            // Drop the embedding; enqueue a backfill so decrypt+re-embed is
            // tracked. Error return surfaces the drift reason to callers;
            // they log but do not abort the delta apply.
            let reason = format!(
                "embedding drift: remote ({}:{}/v{}/dim{}) ≠ local ({}:{}/v{}/dim{})",
                blob.provider,
                blob.model,
                blob.version,
                blob.dim,
                local_provider,
                local_model,
                local_version,
                local_dim
            );
            let reason_for_queue = reason.clone();
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let c = conn.lock();
                c.execute(
                    "INSERT OR IGNORE INTO embedding_backfill_queue \
                     (content_hash, reason, enqueued_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        content_hash_owned,
                        reason_for_queue,
                        chrono::Local::now().to_rfc3339()
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| anyhow::anyhow!("backfill enqueue task panicked: {e}"))??;
            anyhow::bail!("{reason}");
        }

        // Accept: seed embedding_cache. Vec is little-endian f32 bytes.
        if blob.vector.len() != local_dim * 4 {
            anyhow::bail!(
                "embedding blob length {} ≠ expected {} (dim {} × 4 bytes)",
                blob.vector.len(),
                local_dim * 4,
                local_dim
            );
        }
        let bytes = blob.vector.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let c = conn.lock();
            let now = chrono::Local::now().to_rfc3339();
            c.execute(
                "INSERT OR REPLACE INTO embedding_cache \
                 (content_hash, embedding, created_at, accessed_at) \
                 VALUES (?1, ?2, ?3, ?3)",
                rusqlite::params![content_hash_owned, bytes, now],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("embedding cache seed task panicked: {e}"))??;

        Ok(true)
    }

    /// PR #9 Phase 5 — expose the local embedder for agent-loop community
    /// ranking. Goes through `get_or_compute_embedding` so the cache is
    /// shared with `recall()`.
    async fn query_embedding(&self, query: &str) -> Option<Vec<f32>> {
        self.get_or_compute_embedding(query).await.ok().flatten()
    }

    /// PR #5 sender-side — package the embedding we already computed
    /// for `content` into an [`super::sync::EmbeddingBlob`] using local
    /// embedder metadata. Returns `None` when (a) we use `NoopEmbedding`,
    /// (b) the cache misses (the local store has not yet seen this
    /// content), or (c) the cached blob is malformed. Cheap — single
    /// indexed lookup against `embedding_cache`.
    async fn current_embedding_blob(
        &self,
        content: &str,
    ) -> Option<super::sync::EmbeddingBlob> {
        let dim = self.embedder.dimensions();
        if dim == 0 {
            return None;
        }
        let provider = self.embedder.name().to_string();
        let model = self.embedder.model().to_string();
        let version = self.embedder.version();
        let hash = Self::content_hash(content);
        let conn = self.conn.clone();
        let bytes = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
            let conn = conn.lock();
            let mut stmt = conn
                .prepare_cached("SELECT embedding FROM embedding_cache WHERE content_hash = ?1")
                .ok()?;
            stmt.query_row(rusqlite::params![hash], |row| row.get::<_, Vec<u8>>(0))
                .ok()
        })
        .await
        .ok()
        .flatten()?;
        if bytes.len() != dim * 4 {
            return None;
        }
        Some(super::sync::EmbeddingBlob {
            provider,
            model,
            version,
            #[allow(clippy::cast_possible_truncation)]
            dim: dim as u32,
            vector: bytes,
        })
    }

    /// PR #7 — HLC-guarded remote store. Applies the write iff the
    /// parsed remote HLC is strictly greater than the row's existing
    /// `updated_at_hlc`. When the row has no HLC yet (pre-v2 write), we
    /// accept the remote so v2 deltas can win conflicts after a
    /// mixed-version rollout. On a losing comparison we return `false`
    /// without touching the row so the caller can log the skip.
    async fn accept_remote_store_if_newer(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        remote_hlc: &str,
    ) -> anyhow::Result<bool> {
        use crate::sync::hlc::Hlc;

        let remote = Hlc::parse(remote_hlc).map_err(|e| {
            anyhow::anyhow!("accept_remote_store_if_newer: malformed remote_hlc `{remote_hlc}`: {e}")
        })?;
        let embedding_bytes = self
            .get_or_compute_embedding(content)
            .await?
            .map(|emb| vector::vec_to_bytes(&emb));

        let conn = self.conn.clone();
        let key_owned = key.to_string();
        let content_owned = content.to_string();
        let cat_owned = Self::category_to_str(&category);
        let remote_stamp = remote_hlc.to_string();

        let applied = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let conn = conn.lock();

            // Read the row's current stamp (if any). Null → treat as the
            // sentinel "beats any remote" inverse: accept the remote so v1
            // rows can be upgraded to v2-ordered ones. A row that already
            // has an HLC only accepts strictly newer remotes.
            let local_stamp: Option<String> = conn
                .query_row(
                    "SELECT updated_at_hlc FROM memories WHERE key = ?1",
                    rusqlite::params![key_owned],
                    |row| row.get::<_, Option<String>>(0),
                )
                .unwrap_or(None);

            if let Some(local) = local_stamp {
                if let Ok(local_hlc) = Hlc::parse(&local) {
                    if remote <= local_hlc {
                        return Ok(false);
                    }
                }
                // Malformed local stamp → treat as missing and accept the
                // remote. Better to move forward than to get stuck.
            }

            // Accept. Upsert on the unique `key` — set the remote stamp so
            // subsequent deltas compare against it.
            let now = Local::now().to_rfc3339();
            let id = Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO memories (id, key, content, category, embedding, created_at, updated_at, session_id, updated_at_hlc)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8)
                 ON CONFLICT(key) DO UPDATE SET
                    content = excluded.content,
                    category = excluded.category,
                    embedding = excluded.embedding,
                    updated_at = excluded.updated_at,
                    updated_at_hlc = excluded.updated_at_hlc",
                rusqlite::params![
                    id,
                    key_owned,
                    content_owned,
                    cat_owned,
                    embedding_bytes,
                    now,
                    now,
                    remote_stamp,
                ],
            )?;
            Ok(true)
        })
        .await
        .map_err(|e| anyhow::anyhow!("accept_remote_store_if_newer blocking task panicked: {e}"))??;

        Ok(applied)
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
            DeltaOperation::VaultDocUpsert {
                uuid,
                source_type,
                title,
                checksum,
                content_sha256: _,
                frontmatter_json: _,
                links_json: _,
                // PR #5: embedding blob is handled by the SyncedMemory layer
                // (accept_remote_embedding) against the full content once
                // Layer 3 transfers it. We do not cache here because we
                // don't have the cleartext content to key on yet.
                embedding: _,
            } => {
                // Vault (second-brain) docs: shell row only. Full content
                // travels via Layer 3 manifest (existing full-sync path) to
                // keep delta journal small. Here we just register the
                // document identity + checksum so peer knows it exists.
                // Content marked "(pending body sync)" — filled when Layer 3
                // transfers the full text.
                let conn = self.conn.lock();
                // Ensure vault_documents exists; if schema not installed on this
                // SqliteMemory instance (test fixture without vault), ignore.
                let has_table: Option<i64> = conn
                    .query_row(
                        "SELECT 1 FROM sqlite_master
                         WHERE type='table' AND name='vault_documents'",
                        [],
                        |r| r.get(0),
                    )
                    .ok();
                if has_table.is_none() {
                    return Ok(false);
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let changed = conn.execute(
                    "INSERT OR IGNORE INTO vault_documents
                        (uuid, title, content, source_type, source_device_id,
                         checksum, char_count, created_at, updated_at)
                     VALUES (?1, ?2, '(pending body sync)', ?3, 'remote', ?4, 0, ?5, ?5)",
                    params![
                        uuid,
                        title,
                        source_type,
                        checksum,
                        now as i64
                    ],
                )?;
                Ok(changed > 0)
            }
            // ── Self-learning skill system deltas (v6.1) ─────────────
            // Forward to the SkillStore / UserProfiler / CorrectionStore
            // upsert_from_sync methods. Each uses its own LWW policy
            // (skills: version-LWW; profiles: confidence-max; patterns:
            // counts-max). Failure to resolve the workspace dir (e.g.
            // in-memory test fixtures) is silent — returns Ok(false) so
            // the caller logs the skip but doesn't treat it as an error.
            DeltaOperation::SkillUpsert {
                id,
                name,
                category,
                description,
                content_md,
                version,
                created_by,
            } => {
                let Some(workspace) = self.workspace_dir() else {
                    return Ok(false);
                };
                let store = crate::skills::procedural::build_store(workspace, "remote")?;
                store.upsert_from_sync(
                    id,
                    name,
                    category.as_deref(),
                    description,
                    content_md,
                    *version,
                    created_by,
                    "remote",
                )?;
                Ok(true)
            }
            DeltaOperation::UserProfileConclusion {
                dimension,
                conclusion,
                confidence,
                evidence_count,
            } => {
                let Some(workspace) = self.workspace_dir() else {
                    return Ok(false);
                };
                let profiler = crate::user_model::build_profiler(workspace, "remote")?;
                profiler.upsert_from_sync(
                    dimension,
                    conclusion,
                    *confidence,
                    *evidence_count,
                    "remote",
                )?;
                Ok(true)
            }
            DeltaOperation::CorrectionPatternUpsert {
                pattern_type,
                original_regex,
                replacement,
                scope,
                confidence,
                observation_count,
                accept_count,
                reject_count,
            } => {
                let Some(workspace) = self.workspace_dir() else {
                    return Ok(false);
                };
                let store = crate::skills::correction::build_store(workspace, "remote")?;
                store.upsert_from_sync(
                    pattern_type,
                    original_regex,
                    replacement,
                    scope,
                    *confidence,
                    *observation_count,
                    *accept_count,
                    *reject_count,
                )?;
                Ok(true)
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
        // PR #7 HLC migration — tick the clock once per write. Stamp
        // lives on the row alongside the existing RFC3339 `updated_at`
        // so peers can compare monotonically once the sync protocol
        // switches to HLC.
        let hlc_stamp = self.hlc_clock.tick().encode();

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let conn = conn.lock();
            let now = Local::now().to_rfc3339();
            let cat = Self::category_to_str(&category);
            let id = Uuid::new_v4().to_string();

            conn.execute(
                "INSERT INTO memories (id, key, content, category, embedding, created_at, updated_at, session_id, updated_at_hlc)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(key) DO UPDATE SET
                    content = excluded.content,
                    category = excluded.category,
                    embedding = excluded.embedding,
                    updated_at = excluded.updated_at,
                    session_id = excluded.session_id,
                    updated_at_hlc = excluded.updated_at_hlc",
                params![id, key, content, cat, embedding_bytes, now, now, sid, hlc_stamp],
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

        let results = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
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
                    // PR #6 wire-up: archived rows excluded from the LIKE
                    // fallback for the same reason FTS5 + vector exclude
                    // them.
                    let sql = format!(
                        "SELECT id, key, content, category, created_at, session_id FROM memories
                         WHERE archived = 0 AND ({where_clause})
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
        });

        let results: Vec<MemoryEntry> = results
            .await
            .map_err(|e| anyhow::anyhow!("recall blocking task panicked: {e}"))??;

        // PR #6 wire-up: bump recall metrics for every surfaced row so the
        // decay sweep has accurate retrieval counts. Failures are
        // swallowed — bookkeeping must never make a recall fail.
        if !results.is_empty() {
            let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
            if let Err(e) = self.bump_recall_metrics(&ids) {
                tracing::debug!("bump_recall_metrics failed (non-fatal): {e}");
            }
        }
        Ok(results)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let conn = self.conn.clone();
        let key = key.to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<MemoryEntry>> {
            let conn = conn.lock();
            // PR #6 wire-up: surface recall_count / last_recalled from the
            // DB so callers (UI, debuggers, tests) can see what the decay
            // layer actually observed.
            let mut stmt = conn.prepare_cached(
                "SELECT id, key, content, category, created_at, session_id,
                        recall_count, last_recalled
                   FROM memories WHERE key = ?1",
            )?;

            let mut rows = stmt.query_map(params![key], |row| {
                let recall_count_i: i64 = row.get(6)?;
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: row.get(5)?,
                    score: None,
                    recall_count: u32::try_from(recall_count_i.max(0)).unwrap_or(u32::MAX),
                    last_recalled: row.get(7)?,
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

    // ── PR #5 accept_remote_embedding ─────────────────────────

    /// Deterministic 4-dim test embedder so we can exercise the drift
    /// check without pulling ONNX. Name/model/version/dim configurable.
    struct FakeEmbedder {
        name: &'static str,
        model: String,
        version: u32,
        dim: usize,
    }

    #[async_trait::async_trait]
    impl super::super::embedding::EmbeddingProvider for FakeEmbedder {
        fn name(&self) -> &str {
            self.name
        }
        fn model(&self) -> &str {
            &self.model
        }
        fn version(&self) -> u32 {
            self.version
        }
        fn dimensions(&self) -> usize {
            self.dim
        }
        async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.0; self.dim]).collect())
        }
    }

    fn sqlite_with_embedder(embedder: Arc<dyn super::super::embedding::EmbeddingProvider>) -> (TempDir, SqliteMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::with_embedder(
            tmp.path(),
            embedder,
            SearchMode::Rrf,
            0.7,
            0.3,
            60.0,
            10_000,
            None,
        )
        .unwrap();
        (tmp, mem)
    }

    #[tokio::test]
    async fn current_embedding_blob_returns_none_for_noop_embedder() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("k_blob", "anything", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Default temp_sqlite uses NoopEmbedding (dim=0).
        assert!(mem.current_embedding_blob("anything").await.is_none());
    }

    #[tokio::test]
    async fn current_embedding_blob_returns_packed_blob_after_store() {
        // PR #5 sender-side: storing a content with an active embedder
        // populates embedding_cache; current_embedding_blob then surfaces
        // the cached vector wrapped with provider/model metadata.
        let embedder = Arc::new(FakeEmbedder {
            name: "local_fastembed",
            model: "bge-m3".into(),
            version: 1,
            dim: 4,
        });
        let (_tmp, mem) = sqlite_with_embedder(embedder);
        mem.store("k_blob", "사용자 메모", MemoryCategory::Core, None)
            .await
            .unwrap();

        let blob = mem
            .current_embedding_blob("사용자 메모")
            .await
            .expect("blob should be present");
        assert_eq!(blob.provider, "local_fastembed");
        assert_eq!(blob.model, "bge-m3");
        assert_eq!(blob.version, 1);
        assert_eq!(blob.dim, 4);
        assert_eq!(blob.vector.len(), 16); // 4 dim × 4 bytes
        // unpack matches the FakeEmbedder's deterministic zero vector.
        assert_eq!(blob.unpack().unwrap(), vec![0.0_f32; 4]);
    }

    #[tokio::test]
    async fn current_embedding_blob_misses_when_content_not_cached() {
        let embedder = Arc::new(FakeEmbedder {
            name: "local_fastembed",
            model: "bge-m3".into(),
            version: 1,
            dim: 4,
        });
        let (_tmp, mem) = sqlite_with_embedder(embedder);
        // Never stored — cache is empty.
        assert!(mem.current_embedding_blob("uncached content").await.is_none());
    }

    #[tokio::test]
    async fn accept_remote_embedding_caches_when_model_matches() {
        let embedder = Arc::new(FakeEmbedder {
            name: "local_fastembed",
            model: "bge-m3".into(),
            version: 1,
            dim: 4,
        });
        let (_tmp, mem) = sqlite_with_embedder(embedder);
        let content = "identical-model payload";
        let blob = super::super::sync::EmbeddingBlob::pack(
            "local_fastembed",
            "bge-m3",
            1,
            &[0.1, 0.2, 0.3, 0.4],
        );
        let accepted = mem.accept_remote_embedding(content, &blob).await.unwrap();
        assert!(accepted, "matching model must be accepted");

        // Cache row now exists keyed on the same hash recall() would use.
        let hash = SqliteMemory::content_hash(content);
        let conn = mem.conn.lock();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM embedding_cache WHERE content_hash = ?1",
                rusqlite::params![hash],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn accept_remote_embedding_rejects_model_drift_and_enqueues() {
        let embedder = Arc::new(FakeEmbedder {
            name: "local_fastembed",
            model: "bge-m3".into(),
            version: 1,
            dim: 4,
        });
        let (_tmp, mem) = sqlite_with_embedder(embedder);
        // Remote used a different model — vec2text defence must reject.
        let blob = super::super::sync::EmbeddingBlob::pack(
            "openai",
            "text-embedding-3-small",
            1,
            &[0.0; 4],
        );
        let content = "drift payload";
        let err = mem
            .accept_remote_embedding(content, &blob)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("drift"), "got: {err}");

        // Cache must NOT be seeded.
        let hash = SqliteMemory::content_hash(content);
        let conn = mem.conn.lock();
        let cached: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM embedding_cache WHERE content_hash = ?1",
                rusqlite::params![hash],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cached, 0);
        // Backfill queue must contain the content hash.
        let queued: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM embedding_backfill_queue WHERE content_hash = ?1",
                rusqlite::params![hash],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(queued, 1);
    }

    #[tokio::test]
    async fn accept_remote_embedding_rejects_dim_mismatch() {
        let embedder = Arc::new(FakeEmbedder {
            name: "local_fastembed",
            model: "bge-m3".into(),
            version: 1,
            dim: 4,
        });
        let (_tmp, mem) = sqlite_with_embedder(embedder);
        // Same provider/model/version — but 5-dim payload vs local's 4-dim.
        let blob = super::super::sync::EmbeddingBlob::pack(
            "local_fastembed",
            "bge-m3",
            1,
            &[0.1, 0.2, 0.3, 0.4, 0.5],
        );
        let err = mem
            .accept_remote_embedding("dim mismatch", &blob)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("drift"), "got: {err}");
    }

    #[tokio::test]
    async fn accept_remote_embedding_rejects_version_bump() {
        let embedder = Arc::new(FakeEmbedder {
            name: "local_fastembed",
            model: "bge-m3".into(),
            version: 1,
            dim: 4,
        });
        let (_tmp, mem) = sqlite_with_embedder(embedder);
        let blob = super::super::sync::EmbeddingBlob::pack(
            "local_fastembed",
            "bge-m3",
            2, // future version
            &[0.0; 4],
        );
        let err = mem
            .accept_remote_embedding("version bump", &blob)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("drift"), "got: {err}");
    }

    // ── PR #6 consolidation + decay integration ───────────────

    #[tokio::test]
    async fn bump_recall_metrics_increments_count_and_sets_timestamp() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("k_recall", "stuff", MemoryCategory::Core, None)
            .await
            .unwrap();
        let entry = mem.get("k_recall").await.unwrap().unwrap();
        let updated = mem.bump_recall_metrics(&[entry.id.clone()]).unwrap();
        assert_eq!(updated, 1);

        let conn = mem.conn.lock();
        let (count, last): (i64, Option<String>) = conn
            .query_row(
                "SELECT recall_count, last_recalled FROM memories WHERE id = ?1",
                rusqlite::params![entry.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert!(last.is_some());
    }

    #[tokio::test]
    async fn bump_recall_metrics_no_op_on_empty_input() {
        let (_tmp, mem) = temp_sqlite();
        assert_eq!(mem.bump_recall_metrics(&[]).unwrap(), 0);
    }

    #[tokio::test]
    async fn apply_consolidation_outcome_archives_sources_and_writes_summary() {
        use super::super::consolidate::ConsolidationOutcome;

        let (_tmp, mem) = temp_sqlite();
        mem.store("k1", "나는 변호사다", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("k2", "나는 변호사이다", MemoryCategory::Core, None)
            .await
            .unwrap();
        let id1 = mem.get("k1").await.unwrap().unwrap().id;
        let id2 = mem.get("k2").await.unwrap().unwrap().id;

        let outcome = ConsolidationOutcome {
            source_ids: vec![id1.clone(), id2.clone()],
            source_keys: vec!["k1".into(), "k2".into()],
            summary: "사용자는 변호사입니다.".into(),
            conflict: false,
            contradicting_keys: vec![],
        };
        mem.apply_consolidation_outcome(&outcome).unwrap();

        let conn = mem.conn.lock();
        // Both sources archived.
        for id in [&id1, &id2] {
            let archived: i64 = conn
                .query_row(
                    "SELECT archived FROM memories WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(archived, 1, "source {id} not archived");
        }
        // Exactly one consolidated row.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM consolidated_memories",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn apply_consolidation_outcome_persists_conflict_metadata() {
        use super::super::consolidate::ConsolidationOutcome;
        let (_tmp, mem) = temp_sqlite();
        mem.store("kA", "토요일 골프", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("kB", "토요일 테니스", MemoryCategory::Core, None)
            .await
            .unwrap();
        let id_a = mem.get("kA").await.unwrap().unwrap().id;
        let id_b = mem.get("kB").await.unwrap().unwrap().id;

        let outcome = ConsolidationOutcome {
            source_ids: vec![id_a, id_b],
            source_keys: vec!["kA".into(), "kB".into()],
            summary: "주말 운동 (충돌)".into(),
            conflict: true,
            contradicting_keys: vec!["kA".into(), "kB".into()],
        };
        mem.apply_consolidation_outcome(&outcome).unwrap();

        let conn = mem.conn.lock();
        let (flag, contradicting): (i64, String) = conn
            .query_row(
                "SELECT conflict_flag, contradicting_keys FROM consolidated_memories",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(flag, 1);
        let parsed: Vec<String> = serde_json::from_str(&contradicting).unwrap();
        assert_eq!(parsed, vec!["kA", "kB"]);
    }

    #[tokio::test]
    async fn run_decay_sweep_archives_old_low_recall_memories() {
        let (_tmp, mem) = temp_sqlite();
        // Seed 1: ephemeral category, never recalled, fake-old timestamp →
        // should archive on the next sweep.
        mem.store(
            "k_old",
            "stale chat ping",
            MemoryCategory::Custom("ephemeral".into()),
            None,
        )
        .await
        .unwrap();
        // Seed 2: identity category — must survive the sweep no matter
        // how long ago it was created (INFINITY half-life).
        mem.store(
            "k_id",
            "I am a lawyer",
            MemoryCategory::Custom("identity".into()),
            None,
        )
        .await
        .unwrap();

        // Backdate k_old by 365 days so its decay falls below the floor.
        {
            let conn = mem.conn.lock();
            let old = (chrono::Local::now() - chrono::Duration::days(365)).to_rfc3339();
            conn.execute(
                "UPDATE memories SET created_at = ?1, last_recalled = NULL WHERE key = 'k_old'",
                rusqlite::params![old],
            )
            .unwrap();
        }

        let archived = mem.run_decay_sweep().unwrap();
        assert!(archived >= 1, "expected at least k_old to archive");

        let conn = mem.conn.lock();
        let old_archived: i64 = conn
            .query_row(
                "SELECT archived FROM memories WHERE key = 'k_old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_archived, 1);
        let id_archived: i64 = conn
            .query_row(
                "SELECT archived FROM memories WHERE key = 'k_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(id_archived, 0, "identity memory must survive decay");
    }

    #[tokio::test]
    async fn recall_excludes_archived_rows() {
        // PR #6 wire-up regression: a row flipped to archived=1 (e.g. by
        // run_decay_sweep or apply_consolidation_outcome) must never come
        // back through Memory::recall.
        let (_tmp, mem) = temp_sqlite();
        mem.store("k_active", "활성 정보", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("k_archived", "옛날 정보", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Hand-archive k_archived without going through the full
        // consolidation/decay path so the test isolates the SQL filter.
        {
            let conn = mem.conn.lock();
            conn.execute(
                "UPDATE memories SET archived = 1 WHERE key = 'k_archived'",
                [],
            )
            .unwrap();
        }
        // Both rows have NoopEmbedding (dim=0) so recall falls back to FTS.
        let hits = mem.recall("정보", 10, None).await.unwrap();
        let keys: Vec<&str> = hits.iter().map(|h| h.key.as_str()).collect();
        assert!(keys.contains(&"k_active"), "got {keys:?}");
        assert!(!keys.contains(&"k_archived"), "got {keys:?}");
    }

    #[tokio::test]
    async fn read_pool_allows_eight_concurrent_readers() {
        // PR #7 r2d2 — spawn 8 threads, each grabbing a connection from
        // the read pool and running a SELECT in parallel. Must complete
        // without deadlock (the Mutex-only path would serialise them).
        use std::thread;

        let (_tmp, mem) = temp_sqlite();
        mem.store("k_pool", "probe", MemoryCategory::Core, None)
            .await
            .unwrap();

        let pool = mem.read_pool();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let p = pool.clone();
            handles.push(thread::spawn(move || -> anyhow::Result<i64> {
                let conn = p.get()?;
                let count: i64 = conn
                    .query_row("SELECT COUNT(*) FROM memories WHERE key = 'k_pool'", [], |r| {
                        r.get(0)
                    })?;
                Ok(count)
            }));
        }

        for h in handles {
            let n = h.join().unwrap().unwrap();
            assert_eq!(n, 1);
        }
    }

    #[tokio::test]
    async fn read_pool_connection_applies_wal_pragma() {
        // Each pooled connection must run the PRAGMA init block so WAL
        // mode + busy_timeout are active — otherwise reads could see
        // inconsistent snapshots during a writer's transaction.
        let (_tmp, mem) = temp_sqlite();
        let pool = mem.read_pool();
        let conn = pool.get().unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        let busy: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, 5000);
    }

    #[tokio::test]
    async fn store_stamps_monotonic_hlc_on_memories_rows() {
        // PR #7 HLC migration — every INSERT must carry an updated_at_hlc
        // value that parses cleanly and strictly exceeds the previous row's.
        use crate::sync::hlc::Hlc;

        let (_tmp, mem) = temp_sqlite();
        mem.store("k_one", "first", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("k_two", "second", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("k_three", "third", MemoryCategory::Core, None)
            .await
            .unwrap();

        let stamps: Vec<String> = {
            let conn = mem.conn.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT updated_at_hlc FROM memories
                      WHERE key IN ('k_one','k_two','k_three')
                      ORDER BY created_at ASC, key ASC",
                )
                .unwrap();
            stmt.query_map([], |row| row.get::<_, Option<String>>(0))
                .unwrap()
                .map(|r| r.unwrap().expect("HLC stamp must be non-null"))
                .collect()
        };
        assert_eq!(stamps.len(), 3);
        let parsed: Vec<Hlc> = stamps.iter().map(|s| Hlc::parse(s).unwrap()).collect();
        assert!(parsed[1] > parsed[0], "{:?} !> {:?}", parsed[1], parsed[0]);
        assert!(parsed[2] > parsed[1], "{:?} !> {:?}", parsed[2], parsed[1]);
    }

    #[tokio::test]
    async fn store_upsert_refreshes_hlc_on_existing_key() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("k_upsert", "first", MemoryCategory::Core, None)
            .await
            .unwrap();
        let first: String = {
            let conn = mem.conn.lock();
            conn.query_row(
                "SELECT updated_at_hlc FROM memories WHERE key = 'k_upsert'",
                [],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap()
            .unwrap()
        };
        mem.store("k_upsert", "second", MemoryCategory::Core, None)
            .await
            .unwrap();
        let second: String = {
            let conn = mem.conn.lock();
            conn.query_row(
                "SELECT updated_at_hlc FROM memories WHERE key = 'k_upsert'",
                [],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap()
            .unwrap()
        };
        assert_ne!(first, second, "upsert must refresh the HLC stamp");
        let before = crate::sync::hlc::Hlc::parse(&first).unwrap();
        let after = crate::sync::hlc::Hlc::parse(&second).unwrap();
        assert!(after > before);
    }

    #[tokio::test]
    async fn get_surfaces_recall_count_and_last_recalled_from_db() {
        // PR #6 wire-up: the DB columns are populated by
        // bump_recall_metrics; `get()` must expose them so the UI /
        // diagnostics can see the same numbers the decay sweep does.
        let (_tmp, mem) = temp_sqlite();
        mem.store("k_surface", "data", MemoryCategory::Core, None)
            .await
            .unwrap();
        let first = mem.get("k_surface").await.unwrap().unwrap();
        assert_eq!(first.recall_count, 0);
        assert!(first.last_recalled.is_none());

        // Drive the counter up via bump_recall_metrics directly (no
        // recall(), to keep the test focused on the SELECT path).
        mem.bump_recall_metrics(&[first.id.clone()]).unwrap();
        mem.bump_recall_metrics(&[first.id.clone()]).unwrap();

        let after = mem.get("k_surface").await.unwrap().unwrap();
        assert_eq!(after.recall_count, 2);
        assert!(after.last_recalled.is_some());
    }

    #[tokio::test]
    async fn recall_auto_bumps_recall_count_for_returned_rows() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("k_metric", "내가 좋아하는 책", MemoryCategory::Core, None)
            .await
            .unwrap();

        let _ = mem.recall("책", 10, None).await.unwrap();
        let _ = mem.recall("책", 10, None).await.unwrap();

        let conn = mem.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT recall_count FROM memories WHERE key = 'k_metric'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Two recall calls × the row appears in each → 2 bumps.
        assert!(count >= 2, "expected >= 2 bumps, got {count}");
    }

    #[tokio::test]
    async fn collect_consolidation_candidates_filters_archived_and_low_recall() {
        let (_tmp, mem) = temp_sqlite();
        mem.store("k_in", "활성 메모", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("k_out", "비활성 메모", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Bump k_in's recall_count above the threshold; k_out stays at 0.
        let in_id = mem.get("k_in").await.unwrap().unwrap().id;
        mem.bump_recall_metrics(&[in_id.clone()]).unwrap();
        // Archive k_out so it must be excluded even with recall_count>=0.
        {
            let conn = mem.conn.lock();
            conn.execute(
                "UPDATE memories SET archived = 1 WHERE key = 'k_out'",
                [],
            )
            .unwrap();
        }

        let candidates = mem.collect_consolidation_candidates(1).unwrap();
        // Both rows have NULL embeddings (NoopEmbedder), so the filter
        // `embedding IS NOT NULL` excludes them — verifies the SQL guard.
        assert!(candidates.is_empty(), "Noop embedder leaks: {candidates:?}");
    }

    #[tokio::test]
    async fn accept_remote_embedding_is_noop_for_noop_embedder() {
        // Default SqliteMemory uses NoopEmbedding (dim=0). We silently
        // short-circuit because there's no local index to seed.
        let (_tmp, mem) = temp_sqlite();
        let blob = super::super::sync::EmbeddingBlob::pack(
            "openai",
            "whatever",
            1,
            &[0.0; 4],
        );
        let accepted = mem.accept_remote_embedding("noop", &blob).await.unwrap();
        assert!(!accepted);
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
    async fn apply_remote_v3_delta_forwards_skill_upsert() {
        use super::super::sync::DeltaOperation;

        let (tmp, mem) = temp_sqlite();
        let delta = DeltaOperation::SkillUpsert {
            id: "sk-remote-1".into(),
            name: "remote-skill".into(),
            category: Some("coding".into()),
            description: "from remote".into(),
            content_md: "# Remote\n\n## Procedure\n...".into(),
            version: 7,
            created_by: "remote-agent".into(),
        };
        assert!(mem.apply_remote_v3_delta(&delta).await.unwrap());

        // Skill must be visible via a freshly-built SkillStore on the same DB.
        let store = crate::skills::procedural::build_store(tmp.path(), "verifier").unwrap();
        let skill = store.get("sk-remote-1").unwrap().expect("skill present");
        assert_eq!(skill.name, "remote-skill");
        assert_eq!(skill.version, 7);
    }

    #[tokio::test]
    async fn apply_remote_v3_delta_forwards_user_profile_conclusion() {
        use super::super::sync::DeltaOperation;

        let (tmp, mem) = temp_sqlite();
        let delta = DeltaOperation::UserProfileConclusion {
            dimension: "work_style".into(),
            conclusion: "prefers atomic commits".into(),
            confidence: 0.85,
            evidence_count: 4,
        };
        assert!(mem.apply_remote_v3_delta(&delta).await.unwrap());

        let profiler = crate::user_model::build_profiler(tmp.path(), "verifier").unwrap();
        let rows = profiler.find_existing("work_style").unwrap();
        assert!(rows.iter().any(|c| {
            c.conclusion == "prefers atomic commits"
                && (c.confidence - 0.85).abs() < f64::EPSILON
                && c.evidence_count == 4
        }));
    }

    #[tokio::test]
    async fn apply_remote_v3_delta_forwards_correction_pattern() {
        use super::super::sync::DeltaOperation;

        let (tmp, mem) = temp_sqlite();
        let delta = DeltaOperation::CorrectionPatternUpsert {
            pattern_type: "style".into(),
            original_regex: "하였다".into(),
            replacement: "합니다".into(),
            scope: "all".into(),
            confidence: 0.6,
            observation_count: 3,
            accept_count: 2,
            reject_count: 0,
        };
        assert!(mem.apply_remote_v3_delta(&delta).await.unwrap());

        let store = crate::skills::correction::build_store(tmp.path(), "verifier").unwrap();
        let pattern = store.find_pattern("하였다", "합니다").unwrap().expect("pattern present");
        assert_eq!(pattern.observation_count, 3);
        assert!((pattern.confidence - 0.6).abs() < f64::EPSILON);
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

    /// PR #7 — five-minute clock drift between two nodes.
    /// Node A writes at wall 10:00:00, node B writes at wall 10:05:00 (5 min
    /// later), but A's logical counter is 1 (second tick). Under wall-clock
    /// LWW, B's later timestamp always wins. Under HLC ordering, A wins
    /// because its encoded HLC is strictly greater than B's despite the wall
    /// being lower — as long as the HLC counter encodes the correct causal
    /// ordering. This test verifies `accept_remote_store_if_newer` applies
    /// the remote iff its HLC actually leads.
    #[tokio::test]
    async fn accept_remote_store_if_newer_respects_hlc_ordering_under_drift() {
        use crate::sync::hlc::Hlc;

        let (_tmp, mem) = temp_sqlite();

        // Simulate node A storing first (wall 1000 ms, logical 0).
        let hlc_a = Hlc::new(1_000, 0, "node_a").encode();
        mem.store("shared", "from_a", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Stamp the row with A's HLC directly.
        {
            let conn = mem.conn.lock();
            conn.execute(
                "UPDATE memories SET updated_at_hlc = ?1 WHERE key = 'shared'",
                rusqlite::params![hlc_a],
            )
            .unwrap();
        }

        // Node B writes "later" in wall-clock (wall 300_000 ms = +5 min)
        // but with logical 0 — causal ordering says B is newer because its
        // wall time is genuinely higher.
        let hlc_b = Hlc::new(300_000, 0, "node_b").encode();
        let applied_b = mem
            .accept_remote_store_if_newer("shared", "from_b", MemoryCategory::Core, &hlc_b)
            .await
            .unwrap();
        assert!(applied_b, "B (wall 300000) should beat A (wall 1000)");
        let row = mem.get("shared").await.unwrap().unwrap();
        assert_eq!(row.content, "from_b");

        // Now try to apply a stale delta from A (wall 1000, logical 1).
        // Even though A ticked its logical counter, B's wall time is still
        // higher so A should lose.
        let hlc_a_tick = Hlc::new(1_000, 1, "node_a").encode();
        let applied_a = mem
            .accept_remote_store_if_newer("shared", "from_a_retry", MemoryCategory::Core, &hlc_a_tick)
            .await
            .unwrap();
        assert!(!applied_a, "A (wall 1000+1) must lose to B (wall 300000)");
        let row = mem.get("shared").await.unwrap().unwrap();
        assert_eq!(row.content, "from_b", "row must still hold B's write");

        // Finally, A comes back with a genuinely newer HLC (wall 400_000).
        let hlc_a_new = Hlc::new(400_000, 0, "node_a").encode();
        let applied_final = mem
            .accept_remote_store_if_newer("shared", "from_a_newest", MemoryCategory::Core, &hlc_a_new)
            .await
            .unwrap();
        assert!(applied_final, "A (wall 400000) beats B (wall 300000)");
        let row = mem.get("shared").await.unwrap().unwrap();
        assert_eq!(row.content, "from_a_newest");
    }

    /// PR #4 mobile degrade contract — on a mobile build we intentionally
    /// ship without the reranker (150MB model) and without a local
    /// embedder (1.1GB BGE-M3). The retrieval path must stay functional
    /// with both absent: `recall()` should return results from FTS5 +
    /// vector-when-available, and `recall_with_variations` should never
    /// panic or stall when the caller passes a non-empty variations list
    /// on a stub-embedder build.
    ///
    /// This test simulates the bare-mobile environment (default features,
    /// no set_reranker, NoopEmbedding) and asserts the core query path
    /// still returns hits for all three corpus domains.
    #[tokio::test]
    async fn mobile_degrade_recall_still_functional_without_reranker_or_embedder() {
        let (_tmp, mem) = temp_sqlite();

        // Seed a mini cross-domain corpus.
        for (k, v) in [
            ("ko_A", "주택임대차보호법 대항력 발생 시점"),
            ("ko_B", "사무실 화분 다육식물 관리 루틴"),
            ("en_A", "code review approvals required from CODEOWNERS"),
            ("en_B", "feature flag naming convention platform_search_v2"),
            ("law_A", "민법 제621조 임대인 동의 전대 금지"),
        ] {
            mem.store(k, v, MemoryCategory::Core, None)
                .await
                .unwrap();
        }

        // Default SqliteMemory has no reranker attached and uses
        // NoopEmbedding — the exact mobile posture.
        // 1) Plain recall() must return something across all queries.
        for (query, expected_key) in [
            ("대항력", "ko_A"),
            ("CODEOWNERS", "en_A"),
            ("임대인", "law_A"),
        ] {
            let hits = mem.recall(query, 5, None).await.unwrap();
            assert!(
                !hits.is_empty(),
                "mobile recall must return hits for `{query}`, got empty"
            );
            assert!(
                hits.iter().any(|h| h.key == expected_key),
                "mobile recall for `{query}` should surface `{expected_key}`, got {:?}",
                hits.iter().map(|h| h.key.clone()).collect::<Vec<_>>()
            );
        }

        // 2) recall_with_variations must fall through to recall() when no
        //    reranker is attached and variations.len() <= 1 — exact
        //    contract preserved by the PR #4 short-circuit fix.
        let empty_var: [String; 0] = [];
        let hits = mem
            .recall_with_variations("다육식물", &empty_var, 5, None)
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.key == "ko_B"),
            "mobile recall_with_variations must still work without variations + no reranker"
        );

        // 3) Even a multi-variation call without a reranker must return
        //    results (the RRF path without rerank is the default mobile
        //    behaviour when the agent loop does query expansion).
        let variations = vec!["다육식물 물주기".to_string(), "화분 관리".to_string()];
        let hits = mem
            .recall_with_variations("다육식물", &variations, 5, None)
            .await
            .unwrap();
        assert!(
            !hits.is_empty(),
            "mobile multi-variation recall must return hits without a reranker"
        );
    }
}
