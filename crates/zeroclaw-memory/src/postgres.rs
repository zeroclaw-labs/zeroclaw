//! PostgreSQL-backed memory implementation.

use super::embeddings::{self, EmbeddingProvider};
use super::traits::{Memory, MemoryCategory, MemoryEntry, normalize_recent_recall_query};
use super::vector;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::{Mutex, RwLock};
use postgres::types::ToSql;
use postgres::{Client, NoTls, Row};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;
use zeroclaw_api::session_keys::sanitize_session_key;
use zeroclaw_config::schema::SearchMode;

/// Candidate fan-out multiplier for the two hybrid recall stages, so
/// `hybrid_merge` sees enough candidates from each modality to fuse a correct
/// top-`limit` (mirrors the SQLite backend's `limit * 2` fan-out).
const HYBRID_FANOUT: usize = 2;

/// Maximum allowed connect timeout (seconds) to avoid unreasonable waits.
const POSTGRES_CONNECT_TIMEOUT_CAP_SECS: u64 = 300;

struct DropOnThread<T: Send + 'static>(Option<T>);

impl<T: Send + 'static> DropOnThread<T> {
    fn new(value: T) -> Self {
        Self(Some(value))
    }
    fn get(&self) -> &T {
        self.0.as_ref().expect("DropOnThread value already taken")
    }
}

impl<T: Send + 'static> Drop for DropOnThread<T> {
    fn drop(&mut self) {
        let Some(value) = self.0.take() else { return };
        // Wrap in ManuallyDrop so the value is NOT dropped on the current
        // thread if spawn fails — ManuallyDrop's own Drop is a no-op.
        let slot = std::mem::ManuallyDrop::new(value);
        if std::thread::Builder::new()
            .name("postgres-client-drop".to_string())
            .spawn(move || drop(std::mem::ManuallyDrop::into_inner(slot)))
            .is_err()
        {
            // The OS refused to spawn a thread. Intentionally leak the value
            // rather than drop it here: postgres::Client::drop calls
            // Runtime::block_on, which panics on a Tokio runtime thread.
            // A controlled leak is preferable to an unrecoverable panic.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "postgres-client-drop thread spawn failed; leaking client to avoid nested-runtime panic"
            );
            // `slot` is ManuallyDrop — T is intentionally not dropped.
        }
    }
}

pub struct PostgresMemory {
    alias: String,
    client: DropOnThread<Arc<Mutex<Client>>>,
    qualified_table: String,
    qualified_agents: String,
    /// Embedding provider behind an `RwLock` so `refresh_embedder` can hot-swap
    /// it in place without rebuilding the memory handle (matches `SqliteMemory`).
    embedder: Arc<RwLock<Arc<dyn EmbeddingProvider>>>,
    vector_weight: f32,
    keyword_weight: f32,
    search_mode: SearchMode,
    /// Whether the `embedding` vector column exists and is usable. Fixed for
    /// the lifetime of the handle (decided at construction by pgvector setup).
    vector_column: bool,
    /// Whether the vector stage may run: needs the column, a live embedder, and
    /// a search mode that uses vectors. Recomputed on every embedder swap.
    vector_ready: AtomicBool,
}

impl PostgresMemory {
    /// Keyword-only constructor. Preserves the historical 7-argument signature
    /// for callers and tests; equivalent to `with_embedder(.., NoopEmbedding, ..)`
    /// with hybrid defaults, so it never embeds and recall stays keyword-only.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        alias: &str,
        db_url: &str,
        schema: &str,
        table: &str,
        connect_timeout_secs: Option<u64>,
        pgvector_enabled: Option<bool>,
        pgvector_dimensions: Option<usize>,
    ) -> Result<Self> {
        Self::with_embedder(
            alias,
            db_url,
            schema,
            table,
            connect_timeout_secs,
            pgvector_enabled,
            pgvector_dimensions,
            Arc::new(embeddings::NoopEmbedding),
            0.7,
            0.3,
            SearchMode::Hybrid,
        )
    }

    /// Full constructor with a live embedding provider and hybrid weights,
    /// mirroring `SqliteMemory::with_embedder`. When `pgvector_enabled` is set
    /// and the extension + column come up, writes embed their content and recall
    /// runs a hybrid vector+keyword search; otherwise it degrades to the
    /// keyword-only path, byte-for-byte identical to the previous behavior.
    #[allow(clippy::too_many_arguments)]
    pub fn with_embedder(
        alias: &str,
        db_url: &str,
        schema: &str,
        table: &str,
        connect_timeout_secs: Option<u64>,
        pgvector_enabled: Option<bool>,
        pgvector_dimensions: Option<usize>,
        embedder: Arc<dyn EmbeddingProvider>,
        vector_weight: f32,
        keyword_weight: f32,
        search_mode: SearchMode,
    ) -> Result<Self> {
        validate_identifier(schema, "storage schema")?;
        validate_identifier(table, "storage table")?;

        let schema_ident = quote_identifier(schema);
        let table_ident = quote_identifier(table);
        let qualified_table = format!("{schema_ident}.{table_ident}");
        let qualified_agents = format!("{schema_ident}.agents");

        let want_vector = pgvector_enabled.unwrap_or(false);
        let pgvector = if want_vector {
            Some(pgvector_dimensions.unwrap_or(1536))
        } else {
            None
        };

        let (client, vector_column) = Self::initialize_client(
            db_url.to_string(),
            connect_timeout_secs,
            schema_ident.clone(),
            qualified_table.clone(),
            pgvector,
        )?;

        if want_vector && !vector_column {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "pgvector extension not available; falling back to keyword-only recall"
            );
        }

        let vector_ready = Self::compute_vector_ready(vector_column, &embedder, &search_mode);

        Ok(Self {
            alias: alias.to_string(),
            client: DropOnThread::new(Arc::new(Mutex::new(client))),
            qualified_table,
            qualified_agents,
            embedder: Arc::new(RwLock::new(embedder)),
            vector_weight,
            keyword_weight,
            search_mode,
            vector_column,
            vector_ready: AtomicBool::new(vector_ready),
        })
    }

    /// The vector stage may run only when the `embedding` column exists, the
    /// embedder actually produces vectors, and the search mode consumes them.
    fn compute_vector_ready(
        vector_column: bool,
        embedder: &Arc<dyn EmbeddingProvider>,
        search_mode: &SearchMode,
    ) -> bool {
        vector_column && embedder.dimensions() > 0 && !matches!(search_mode, SearchMode::Bm25)
    }

    /// Swap the live embedder in place and recompute the vector-stage gate.
    /// Shared by the runtime `refresh_embedder` hook and tests injecting a fake
    /// provider. Existing `Arc<dyn Memory>` holders observe the new embedder on
    /// their next embed without rebuilding the handle.
    pub fn swap_embedder(&self, embedder: Arc<dyn EmbeddingProvider>) {
        let ready = Self::compute_vector_ready(self.vector_column, &embedder, &self.search_mode);
        *self.embedder.write() = embedder;
        self.vector_ready.store(ready, Ordering::Release);
    }

    /// Snapshot the current embedder so a concurrent `refresh_embedder` swap
    /// cannot race an in-flight embed.
    fn current_embedder(&self) -> Arc<dyn EmbeddingProvider> {
        self.embedder.read().clone()
    }

    /// Compute an embedding for `text`, returning `None` when the embedder is a
    /// no-op or the provider call fails (the row is then persisted without a
    /// vector, matching the SQLite backend's degrade-but-store behavior).
    async fn embed_if_live(&self, text: &str) -> Option<Vec<f32>> {
        if !self.vector_ready.load(Ordering::Acquire) {
            return None;
        }
        let embedder = self.current_embedder();
        if embedder.dimensions() == 0 {
            return None;
        }
        match embedder.embed_one(text).await {
            Ok(v) => Some(v),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "memory store: embedding failed; persisting row without a vector \
                     (run `zeroclaw memory reindex` to backfill once the embedder recovers)"
                );
                None
            }
        }
    }

    fn initialize_client(
        db_url: String,
        connect_timeout_secs: Option<u64>,
        schema_ident: String,
        qualified_table: String,
        pgvector: Option<usize>,
    ) -> Result<(Client, bool)> {
        let init_handle = std::thread::Builder::new()
            .name("postgres-memory-init".to_string())
            .spawn(move || -> Result<(Client, bool)> {
                let mut config: postgres::Config = db_url
                    .parse()
                    .context("invalid PostgreSQL connection URL")?;

                if let Some(timeout_secs) = connect_timeout_secs {
                    let bounded = timeout_secs.min(POSTGRES_CONNECT_TIMEOUT_CAP_SECS);
                    config.connect_timeout(Duration::from_secs(bounded));
                }

                let mut client = config
                    .connect(NoTls)
                    .context("failed to connect to PostgreSQL memory backend")?;

                Self::init_schema(&mut client, &schema_ident, &qualified_table)?;
                zeroclaw_config::schema::v2::migrate_postgres_memory_to_v3(
                    &mut client,
                    &schema_ident,
                    &qualified_table,
                )?;
                // The synchronous postgres client uses block_on internally,
                // so pgvector setup must also stay on this plain OS thread —
                // calling it from a Tokio runtime thread panics.
                let vector_column = match pgvector {
                    Some(dimensions) => {
                        Self::try_enable_pgvector(&mut client, &qualified_table, dimensions).is_ok()
                    }
                    None => false,
                };
                Ok((client, vector_column))
            })
            .context("failed to spawn PostgreSQL initializer thread")?;

        init_handle.join().map_err(|_| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "PostgreSQL initializer thread panicked"
            );
            anyhow::Error::msg("PostgreSQL initializer thread panicked")
        })?
    }

    fn init_schema(client: &mut Client, schema_ident: &str, qualified_table: &str) -> Result<()> {
        client.batch_execute(&format!(
            "
            CREATE SCHEMA IF NOT EXISTS {schema_ident};

            CREATE TABLE IF NOT EXISTS {qualified_table} (
                id TEXT PRIMARY KEY,
                key TEXT NOT NULL,
                content TEXT NOT NULL,
                category TEXT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL,
                session_id TEXT
            );
            -- Composite (agent_id, key) uniqueness lands in the V3 migration
            -- once the `agent_id` column is added and backfilled.

            CREATE INDEX IF NOT EXISTS idx_memories_category ON {qualified_table}(category);
            CREATE INDEX IF NOT EXISTS idx_memories_session_id ON {qualified_table}(session_id);
            CREATE INDEX IF NOT EXISTS idx_memories_updated_at ON {qualified_table}(updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_memories_content_fts ON {qualified_table} USING gin(to_tsvector('simple', content));
            CREATE INDEX IF NOT EXISTS idx_memories_key_fts ON {qualified_table} USING gin(to_tsvector('simple', key));
            "
        ))?;

        Self::migrate_session_ids_to_sanitized(client, qualified_table)?;

        Ok(())
    }

    fn migrate_session_ids_to_sanitized(client: &mut Client, qualified_table: &str) -> Result<()> {
        let select = format!(
            "SELECT DISTINCT session_id FROM {qualified_table} WHERE session_id IS NOT NULL"
        );
        let rows = client.query(&select, &[])?;
        let distinct: Vec<String> = rows.iter().map(|r| r.get(0)).collect();

        let rewrites = Self::compute_session_id_rewrites(&distinct);
        if rewrites.is_empty() {
            return Ok(());
        }

        let update = format!("UPDATE {qualified_table} SET session_id = $1 WHERE session_id = $2");
        let stmt = client.prepare(&update)?;
        for (old, new) in &rewrites {
            client.execute(&stmt, &[new, old])?;
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"rewritten": rewrites.len()})),
            "Normalized session_id values in memories table to sanitized form"
        );

        Ok(())
    }

    /// Pure plan of `(old, new)` `session_id` rewrites for the rows whose
    /// stored value differs from its sanitized form. Extracted from
    /// `migrate_session_ids_to_sanitized` so the rewrite logic is
    /// unit-testable without a live PostgreSQL instance.
    fn compute_session_id_rewrites(distinct: &[String]) -> Vec<(String, String)> {
        distinct
            .iter()
            .filter_map(|old| {
                let new = sanitize_session_key(old);
                if new == *old {
                    None
                } else {
                    Some((old.clone(), new))
                }
            })
            .collect()
    }

    fn category_to_str(category: &MemoryCategory) -> String {
        match category {
            MemoryCategory::Core => "core".to_string(),
            MemoryCategory::Daily => "daily".to_string(),
            MemoryCategory::Conversation => "conversation".to_string(),
            MemoryCategory::Custom(name) => name.clone(),
        }
    }

    fn parse_category(value: &str) -> MemoryCategory {
        match value {
            "core" => MemoryCategory::Core,
            "daily" => MemoryCategory::Daily,
            "conversation" => MemoryCategory::Conversation,
            other => MemoryCategory::Custom(other.to_string()),
        }
    }

    fn try_enable_pgvector(
        client: &mut Client,
        qualified_table: &str,
        dimensions: usize,
    ) -> Result<()> {
        client.batch_execute("CREATE EXTENSION IF NOT EXISTS vector")?;
        client.batch_execute(&format!(
            r#"
            DO $$ BEGIN
                ALTER TABLE {qualified_table} ADD COLUMN IF NOT EXISTS namespace TEXT DEFAULT 'default';
                ALTER TABLE {qualified_table} ADD COLUMN IF NOT EXISTS importance REAL;
                ALTER TABLE {qualified_table} ADD COLUMN IF NOT EXISTS embedding vector({dimensions});
            EXCEPTION WHEN OTHERS THEN
                RAISE NOTICE 'pgvector columns could not be added: %', SQLERRM;
            END $$;
            CREATE INDEX IF NOT EXISTS idx_memories_namespace ON {qualified_table}(namespace);
            "#
        ))?;
        Ok(())
    }

    fn row_to_entry(row: &Row) -> Result<MemoryEntry> {
        // Named access is used throughout so row_to_entry is immune to SELECT
        // column reordering and does not depend on matching the DDL ordering.
        let timestamp: DateTime<Utc> = row.get("created_at");

        Ok(MemoryEntry {
            principal_id: None,
            id: row.get("id"),
            key: row.get("key"),
            content: row.get("content"),
            category: Self::parse_category(&row.get::<_, String>("category")),
            timestamp: timestamp.to_rfc3339(),
            session_id: row.get("session_id"),
            score: row.try_get("score").ok(),
            namespace: row
                .try_get::<_, String>("namespace")
                .unwrap_or_else(|_| "default".into()),
            importance: row.try_get("importance").ok(),
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: row.try_get("agent_alias").ok(),
            agent_id: row.try_get("agent_id").ok(),
        })
    }

    /// Persist a single embedding vector for an existing row (used by `reindex`).
    async fn write_embedding(
        &self,
        client: &Arc<Mutex<Client>>,
        qualified_table: &str,
        id: &str,
        v: &[f32],
    ) -> Result<()> {
        let client = client.clone();
        let qualified_table = qualified_table.to_string();
        let lit = vector::vec_to_pgvector(v);
        let id = id.to_string();
        run_on_os_thread(move || -> Result<()> {
            let mut client = client.lock();
            let stmt =
                format!("UPDATE {qualified_table} SET embedding = $2::text::vector WHERE id = $1");
            client.execute(&stmt, &[&id, &lit])?;
            Ok(())
        })
        .await
    }
    // ── Hybrid (vector + keyword) recall helpers ───────────────────────

    /// Build the shared `WHERE` fragment — session, optional agent allowlist,
    /// and created-at window — for a recall stage, assigning `$`-placeholder
    /// indices from `start`. Returns `(fragment, next_free_index)` so the caller
    /// can append its `LIMIT` param. Params must be pushed in the same order the
    /// clauses appear (session, agent, since, until).
    fn scoped_where(
        start: usize,
        has_session: bool,
        has_agent: bool,
        since: bool,
        until: bool,
    ) -> (String, usize) {
        let mut idx = start;
        let mut clauses = String::new();
        if has_session {
            clauses.push_str(&format!(" AND m.session_id = ${idx}"));
            idx += 1;
        }
        if has_agent {
            clauses.push_str(&format!(" AND m.agent_id = ANY(${idx})"));
            idx += 1;
        }
        if since {
            clauses.push_str(&format!(" AND m.created_at >= ${idx}::TIMESTAMPTZ"));
            idx += 1;
        }
        if until {
            clauses.push_str(&format!(" AND m.created_at <= ${idx}::TIMESTAMPTZ"));
            idx += 1;
        }
        (clauses, idx)
    }

    /// Keyword (FTS) recall stage — the semantic equivalent of the historical
    /// keyword-only path. The empty-query branch (`$1 = ''`) preserves the prior
    /// recency ordering. `limit` caps the returned rows.
    async fn keyword_entries(
        &self,
        allowed: Option<&[String]>,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let query = query.to_string();
        let sid = session_id.map(str::to_string);
        let since_owned = since.map(str::to_string);
        let until_owned = until.map(str::to_string);
        let allowed = allowed.map(<[String]>::to_vec);

        #[allow(clippy::cast_possible_wrap)]
        let limit_i64 = limit as i64;

        let has_v3_cols = self.vector_column;

        run_on_os_thread(move || -> Result<Vec<MemoryEntry>> {
            let has_session = sid.is_some();
            let has_agent = allowed.is_some();
            let since = since_owned.is_some();
            let until = until_owned.is_some();
            let (where_frag, next_idx) =
                Self::scoped_where(2, has_session, has_agent, since, until);
            let v3_cols = if has_v3_cols {
                "m.namespace, m.importance, "
            } else {
                ""
            };

            let stmt = format!(
                "
                SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, \
                       {v3_cols}a.alias AS agent_alias, m.agent_id,
                       (
                         CASE WHEN to_tsvector('simple', m.key) @@ plainto_tsquery('simple', $1)
                           THEN ts_rank_cd(to_tsvector('simple', m.key), plainto_tsquery('simple', $1)) * 2.0
                           ELSE 0.0 END +
                         CASE WHEN to_tsvector('simple', m.content) @@ plainto_tsquery('simple', $1)
                           THEN ts_rank_cd(to_tsvector('simple', m.content), plainto_tsquery('simple', $1))
                           ELSE 0.0 END
                       ) AS score
                FROM {qualified_table} m
                LEFT JOIN {qualified_agents} a ON a.id = m.agent_id
                WHERE ($1 = '' OR to_tsvector('simple', m.key || ' ' || m.content) @@ plainto_tsquery('simple', $1))
                  {where_frag}
                ORDER BY score DESC, m.updated_at DESC
                LIMIT ${next_idx}
                "
            );

            let mut client = client.lock();
            let mut params: Vec<Box<dyn ToSql + Sync>> = Vec::new();
            params.push(Box::new(query));
            if has_session {
                params.push(Box::new(sid.clone()));
            }
            if has_agent {
                params.push(Box::new(allowed.clone().unwrap_or_default()));
            }
            if since {
                params.push(Box::new(since_owned.clone()));
            }
            if until {
                params.push(Box::new(until_owned.clone()));
            }
            params.push(Box::new(limit_i64));
            let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| &**p as &(dyn ToSql + Sync)).collect();

            let rows = client.query(&stmt, refs.as_slice())?;
            rows.iter().map(Self::row_to_entry).collect()
        })
        .await
    }

    /// Vector (cosine) recall stage: nearest neighbours to `qvec` among rows
    /// that have an embedding, honoring the same session/agent/time scoping as
    /// the keyword stage. Returns `(id, cosine_similarity)` in descending
    /// similarity order. Only invoked when the vector stage is live.
    async fn vector_hits(
        &self,
        allowed: Option<&[String]>,
        qvec: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<(String, f32)>> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let qvec = qvec.to_string();
        let sid = session_id.map(str::to_string);
        let since_owned = since.map(str::to_string);
        let until_owned = until.map(str::to_string);
        let allowed = allowed.map(<[String]>::to_vec);

        #[allow(clippy::cast_possible_wrap)]
        let limit_i64 = limit as i64;

        run_on_os_thread(move || -> Result<Vec<(String, f32)>> {
            let has_session = sid.is_some();
            let has_agent = allowed.is_some();
            let since = since_owned.is_some();
            let until = until_owned.is_some();
            let (where_frag, next_idx) =
                Self::scoped_where(2, has_session, has_agent, since, until);

            let stmt = format!(
                "
                SELECT m.id, GREATEST(0, 1 - (m.embedding <=> $1::text::vector))::float8 AS score
                FROM {qualified_table} m
                WHERE m.embedding IS NOT NULL
                  {where_frag}
                ORDER BY m.embedding <=> $1::text::vector
                LIMIT ${next_idx}
                "
            );

            let mut client = client.lock();
            let mut params: Vec<Box<dyn ToSql + Sync>> = Vec::new();
            params.push(Box::new(qvec));
            if has_session {
                params.push(Box::new(sid.clone()));
            }
            if has_agent {
                params.push(Box::new(allowed.clone().unwrap_or_default()));
            }
            if since {
                params.push(Box::new(since_owned.clone()));
            }
            if until {
                params.push(Box::new(until_owned.clone()));
            }
            params.push(Box::new(limit_i64));
            let refs: Vec<&(dyn ToSql + Sync)> =
                params.iter().map(|p| &**p as &(dyn ToSql + Sync)).collect();

            let rows = client.query(&stmt, refs.as_slice())?;
            Ok(rows
                .iter()
                .map(|r| {
                    let id: String = r.get("id");
                    let score: f64 = r.get("score");
                    #[allow(clippy::cast_possible_truncation)]
                    let score = score as f32;
                    (id, score)
                })
                .collect())
        })
        .await
    }

    /// Fetch full entries for a set of ids (used to materialize the merged
    /// hybrid ranking without an N+1 query pattern).
    async fn entries_for_ids(&self, ids: &[String]) -> Result<HashMap<String, MemoryEntry>> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let ids = ids.to_vec();

        run_on_os_thread(move || -> Result<HashMap<String, MemoryEntry>> {
            let mut client = client.lock();
            let stmt = format!(
                "
                SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, \
                       m.namespace, m.importance, a.alias AS agent_alias, m.agent_id
                FROM {qualified_table} m
                LEFT JOIN {qualified_agents} a ON a.id = m.agent_id
                WHERE m.id = ANY($1)
                "
            );
            let rows = client.query(&stmt, &[&ids])?;
            let mut map = HashMap::with_capacity(rows.len());
            for row in &rows {
                let entry = Self::row_to_entry(row)?;
                map.insert(entry.id.clone(), entry);
            }
            Ok(map)
        })
        .await
    }

    /// Unified recall used by both `recall` and `recall_for_agents`.
    ///
    /// When the vector stage is live (column present, embedder produces a query
    /// vector, non-empty query, mode uses vectors) it runs a two-stage hybrid:
    /// cosine neighbours fused with the keyword ranking via `hybrid_merge`.
    /// Otherwise it returns the keyword stage verbatim — byte-for-byte the prior
    /// keyword-only behavior — so a no-op embedder or a `bm25` mode is a strict
    /// no-op relative to the previous implementation.
    async fn recall_core(
        &self,
        allowed: Option<&[String]>,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let allowed = allowed.filter(|v| !v.is_empty());
        let q = normalize_recent_recall_query(query).trim().to_string();

        let qvec = if self.vector_ready.load(Ordering::Acquire)
            && !q.is_empty()
            && !matches!(self.search_mode, SearchMode::Bm25)
        {
            self.embed_if_live(&q)
                .await
                .map(|v| vector::vec_to_pgvector(&v))
        } else {
            None
        };

        // No live vector stage → exact prior behavior.
        let Some(qvec) = qvec else {
            return self
                .keyword_entries(allowed, &q, limit, session_id, since, until)
                .await;
        };

        let candidate_limit = limit.saturating_mul(HYBRID_FANOUT).max(10);

        // Keyword stage (skipped in embedding-only mode).
        let kw_entries = if matches!(self.search_mode, SearchMode::Embedding) {
            Vec::new()
        } else {
            self.keyword_entries(allowed, &q, candidate_limit, session_id, since, until)
                .await?
        };
        let kw_results: Vec<(String, f32)> = kw_entries
            .iter()
            .filter_map(|e| e.score.map(|s| (e.id.clone(), s as f32)))
            .collect();

        // Vector stage (always present here since qvec is Some).
        let vec_hits = self
            .vector_hits(allowed, &qvec, candidate_limit, session_id, since, until)
            .await?;

        let merged: Vec<vector::ScoredResult> = if vec_hits.is_empty() {
            super::normalize::bm25_to_unit(&kw_results)
                .into_iter()
                .map(|(id, s)| vector::ScoredResult {
                    id,
                    vector_score: None,
                    keyword_score: Some(s),
                    final_score: s,
                })
                .collect()
        } else if kw_results.is_empty() {
            vec_hits
                .into_iter()
                .map(|(id, s)| vector::ScoredResult {
                    id,
                    vector_score: Some(s),
                    keyword_score: None,
                    final_score: s,
                })
                .collect()
        } else {
            vector::hybrid_merge(
                &vec_hits,
                &kw_results,
                self.vector_weight,
                self.keyword_weight,
                limit,
            )
        };

        let scored: Vec<(String, f32)> = merged
            .into_iter()
            .take(limit)
            .map(|m| (m.id, m.final_score))
            .collect();
        if scored.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = scored.iter().map(|(id, _)| id.clone()).collect();
        let map = self.entries_for_ids(&ids).await?;
        let mut out = Vec::with_capacity(scored.len());
        for (id, final_score) in &scored {
            if let Some(mut e) = map.get(id).cloned() {
                e.score = Some(f64::from(*final_score));
                out.push(e);
            }
        }
        Ok(out)
    }
}

async fn run_on_os_thread<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = oneshot::channel();

    std::thread::Builder::new()
        .name("postgres-memory-op".to_string())
        .spawn(move || {
            let result = f();
            let _ = tx.send(result);
        })
        .context("failed to spawn PostgreSQL operation thread")?;

    rx.await.map_err(|_| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure),
            "PostgreSQL operation thread terminated unexpectedly"
        );
        anyhow::Error::msg("PostgreSQL operation thread terminated unexpectedly")
    })?
}

pub(super) fn validate_identifier(value: &str, field_name: &str) -> Result<()> {
    if value.is_empty() {
        anyhow::bail!("{field_name} must not be empty");
    }

    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        anyhow::bail!("{field_name} must not be empty");
    };

    if !(first.is_ascii_alphabetic() || first == '_') {
        anyhow::bail!("{field_name} must start with an ASCII letter or underscore; got '{value}'");
    }

    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        anyhow::bail!(
            "{field_name} can only contain ASCII letters, numbers, and underscores; got '{value}'"
        );
    }

    Ok(())
}

pub(super) fn quote_identifier(value: &str) -> String {
    format!("\"{value}\"")
}

#[async_trait]
impl Memory for PostgresMemory {
    fn name(&self) -> &str {
        "postgres"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
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
    ) -> Result<Vec<MemoryEntry>> {
        self.recall_core(None, query, limit, session_id, since, until)
            .await
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let key = key.to_string();

        run_on_os_thread(move || -> Result<Option<MemoryEntry>> {
            let mut client = client.lock();
            let stmt = format!(
                "
                SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, a.alias AS agent_alias, m.agent_id
                FROM {qualified_table} m
                LEFT JOIN {qualified_agents} a ON a.id = m.agent_id
                WHERE m.key = $1
                LIMIT 1
                "
            );

            let row = client.query_opt(&stmt, &[&key])?;
            row.as_ref().map(Self::row_to_entry).transpose()
        })
        .await
    }

    async fn get_for_agent(&self, key: &str, agent_id: &str) -> Result<Option<MemoryEntry>> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let key = key.to_string();
        let agent_id = agent_id.to_string();

        run_on_os_thread(move || -> Result<Option<MemoryEntry>> {
            let mut client = client.lock();
            let stmt = format!(
                "
                SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, a.alias AS agent_alias, m.agent_id
                FROM {qualified_table} m
                LEFT JOIN {qualified_agents} a ON a.id = m.agent_id
                WHERE m.key = $1 AND m.agent_id = $2
                LIMIT 1
                "
            );

            let row = client.query_opt(&stmt, &[&key, &agent_id])?;
            row.as_ref().map(Self::row_to_entry).transpose()
        })
        .await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let category = category.map(Self::category_to_str);
        let sid = session_id.map(str::to_string);

        run_on_os_thread(move || -> Result<Vec<MemoryEntry>> {
            let mut client = client.lock();
            let stmt = format!(
                "
                SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, a.alias AS agent_alias, m.agent_id
                FROM {qualified_table} m
                LEFT JOIN {qualified_agents} a ON a.id = m.agent_id
                WHERE ($1::TEXT IS NULL OR m.category = $1)
                  AND ($2::TEXT IS NULL OR m.session_id = $2)
                ORDER BY m.updated_at DESC
                "
            );

            let category_ref = category.as_deref();
            let session_ref = sid.as_deref();
            let rows = client.query(&stmt, &[&category_ref, &session_ref])?;
            rows.iter()
                .map(Self::row_to_entry)
                .collect::<Result<Vec<MemoryEntry>>>()
        })
        .await
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let key = key.to_string();

        run_on_os_thread(move || -> Result<bool> {
            let mut client = client.lock();
            let stmt = format!("DELETE FROM {qualified_table} WHERE key = $1");
            let deleted = client.execute(&stmt, &[&key])?;
            Ok(deleted > 0)
        })
        .await
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> Result<bool> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let key = key.to_string();
        let agent_id = agent_id.to_string();

        run_on_os_thread(move || -> Result<bool> {
            let mut client = client.lock();
            let stmt = format!("DELETE FROM {qualified_table} WHERE key = $1 AND agent_id = $2");
            let deleted = client.execute(&stmt, &[&key, &agent_id])?;
            Ok(deleted > 0)
        })
        .await
    }

    async fn purge_session_for_agent(&self, session_id: &str, agent_id: &str) -> Result<usize> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();

        run_on_os_thread(move || -> Result<usize> {
            let mut client = client.lock();
            let stmt =
                format!("DELETE FROM {qualified_table} WHERE session_id = $1 AND agent_id = $2");
            let deleted = client.execute(&stmt, &[&session_id, &agent_id])?;
            usize::try_from(deleted).context("PostgreSQL returned an oversized delete count")
        })
        .await
    }

    async fn purge_agent(&self, agent_alias: &str) -> Result<usize> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let alias = agent_alias.to_string();

        run_on_os_thread(move || -> Result<usize> {
            let mut client = client.lock();
            let stmt = format!(
                "DELETE FROM {qualified_table} WHERE agent_id = (SELECT id FROM {qualified_agents} WHERE alias = $1)"
            );
            let deleted = client.execute(&stmt, &[&alias])?;
            usize::try_from(deleted).context("PostgreSQL returned an oversized delete count")
        })
        .await
    }

    async fn export_agent(&self, agent_alias: &str) -> Result<Vec<MemoryEntry>> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let alias = agent_alias.to_string();

        run_on_os_thread(move || -> Result<Vec<MemoryEntry>> {
            let mut client = client.lock();
            let stmt = format!(
                "
                SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, a.alias AS agent_alias, m.agent_id
                FROM {qualified_table} m
                LEFT JOIN {qualified_agents} a ON a.id = m.agent_id
                WHERE m.agent_id = (SELECT id FROM {qualified_agents} WHERE alias = $1)
                ORDER BY m.created_at ASC
                "
            );
            let rows = client.query(&stmt, &[&alias])?;
            rows.iter()
                .map(Self::row_to_entry)
                .collect::<Result<Vec<MemoryEntry>>>()
        })
        .await
    }

    async fn rename_agent(&self, from: &str, to: &str) -> Result<usize> {
        let client = self.client.get().clone();
        let qualified_agents = self.qualified_agents.clone();
        let qualified_table = self.qualified_table.clone();
        let from = from.to_string();
        let to = to.to_string();

        run_on_os_thread(move || -> Result<usize> {
            let mut client = client.lock();
            let mut tx = client.transaction()?;
            let to_rows: i64 = tx
                .query_one(
                    &format!(
                        "SELECT COUNT(*) FROM {qualified_table} WHERE agent_id = (SELECT id FROM {qualified_agents} WHERE alias = $1)"
                    ),
                    &[&to],
                )?
                .get(0);
            if to_rows > 0 {
                anyhow::bail!(
                    "cannot rename agent memory to `{to}`: an existing memory store under that alias has {to_rows} row(s); refusing to merge"
                );
            }
            tx.execute(
                &format!("DELETE FROM {qualified_agents} WHERE alias = $1"),
                &[&to],
            )?;
            let updated = tx.execute(
                &format!("UPDATE {qualified_agents} SET alias = $2 WHERE alias = $1"),
                &[&from, &to],
            )?;
            tx.commit()?;
            usize::try_from(updated).context("PostgreSQL returned an oversized update count")
        })
        .await
    }

    async fn count_agent(&self, agent_alias: &str) -> Result<usize> {
        let client = self.client.get().clone();
        let qualified_agents = self.qualified_agents.clone();
        let alias = agent_alias.to_string();

        run_on_os_thread(move || -> Result<usize> {
            let mut client = client.lock();
            // Mirror `rename_agent`: it moves the `agents` row (alias -> id), so
            // residue is the presence of that alias row (0 or 1), NOT the memory-
            // row count (which would miss an agents-row-only lag).
            let stmt = format!("SELECT COUNT(*) FROM {qualified_agents} WHERE alias = $1");
            let row = client.query_one(&stmt, &[&alias])?;
            let count: i64 = row.get(0);
            usize::try_from(count).context("PostgreSQL returned an oversized agent count")
        })
        .await
    }

    async fn count(&self) -> Result<usize> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();

        run_on_os_thread(move || -> Result<usize> {
            let mut client = client.lock();
            let stmt = format!("SELECT COUNT(*) FROM {qualified_table}");
            let count: i64 = client.query_one(&stmt, &[])?.get(0);
            let count =
                usize::try_from(count).context("PostgreSQL returned a negative memory count")?;
            Ok(count)
        })
        .await
    }

    async fn health_check(&self) -> bool {
        let client = self.client.get().clone();
        run_on_os_thread(move || Ok(client.lock().simple_query("SELECT 1").is_ok()))
            .await
            .unwrap_or(false)
    }

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> Result<()> {
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let key = key.to_string();
        let content = content.to_string();
        let category = Self::category_to_str(&category);
        let sid = session_id.map(str::to_string);
        let aid = agent_id.map(str::to_string);

        // Embed on write when a vector column + live embedder are present.
        // A provider failure degrades to a NULL vector (the row still stores);
        // `reindex` can backfill it later.
        let embedding = self.embed_if_live(&content).await;
        let emb_lit: Option<String> = embedding.as_ref().map(|v| vector::vec_to_pgvector(v));
        let vector_column = self.vector_column;

        run_on_os_thread(move || -> Result<()> {
            let now = Utc::now();
            let mut client = client.lock();
            let id = Uuid::new_v4().to_string();

            let rows = if vector_column {
                let set_embedding = if emb_lit.is_some() {
                    ", embedding = EXCLUDED.embedding"
                } else {
                    ""
                };
                let stmt = format!(
                    "
                    INSERT INTO {qualified_table}
                        (id, key, content, category, created_at, updated_at, session_id, agent_id, embedding)
                    VALUES
                        ($1, $2, $3, $4, $5, $6, $7,
                         COALESCE($8, (SELECT id FROM {qualified_agents} WHERE alias = 'default' LIMIT 1)),
                         $9::text::vector)
                    ON CONFLICT (agent_id, key) DO UPDATE SET
                        content = EXCLUDED.content,
                        category = EXCLUDED.category,
                        updated_at = EXCLUDED.updated_at,
                        session_id = EXCLUDED.session_id{set_embedding}
                    "
                );
                let params: Vec<Box<dyn ToSql + Sync>> = vec![
                    Box::new(id),
                    Box::new(key),
                    Box::new(content),
                    Box::new(category),
                    Box::new(now),
                    Box::new(now),
                    Box::new(sid),
                    Box::new(aid),
                    Box::new(emb_lit),
                ];
                let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| &**p as &(dyn ToSql + Sync)).collect();
                client.execute(&stmt, refs.as_slice())?
            } else {
                let stmt = format!(
                    "
                    INSERT INTO {qualified_table}
                        (id, key, content, category, created_at, updated_at, session_id, agent_id)
                    VALUES
                        ($1, $2, $3, $4, $5, $6, $7,
                         COALESCE($8, (SELECT id FROM {qualified_agents} WHERE alias = 'default' LIMIT 1)))
                    ON CONFLICT (agent_id, key) DO UPDATE SET
                        content = EXCLUDED.content,
                        category = EXCLUDED.category,
                        updated_at = EXCLUDED.updated_at,
                        session_id = EXCLUDED.session_id
                    "
                );
                let params: Vec<Box<dyn ToSql + Sync>> = vec![
                    Box::new(id),
                    Box::new(key),
                    Box::new(content),
                    Box::new(category),
                    Box::new(now),
                    Box::new(now),
                    Box::new(sid),
                    Box::new(aid),
                ];
                let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| &**p as &(dyn ToSql + Sync)).collect();
                client.execute(&stmt, refs.as_slice())?
            };
            let _ = rows;
            Ok(())
        })
        .await
    }

    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Empty allowlist means "no agent filter": fall back to plain
        // recall. The wrapper always includes the bound agent's UUID,
        // so a non-empty allowlist is the live-runtime case.
        if allowed_agent_ids.is_empty() {
            return self.recall(query, limit, session_id, since, until).await;
        }
        let allowed: Vec<String> = allowed_agent_ids.iter().map(|s| (*s).to_string()).collect();
        self.recall_core(
            Some(allowed.as_slice()),
            query,
            limit,
            session_id,
            since,
            until,
        )
        .await
    }

    async fn reindex(&self) -> Result<usize> {
        // 1) Rebuild the derived indexes. The keyword stage is a GIN expression
        //    index over to_tsvector, which PostgreSQL keeps current on write, so
        //    a plain REINDEX is the correct "rebuild" for bulk writes that
        //    bypassed `store()` (e.g. migrate/import).
        {
            let client = self.client.get().clone();
            let qualified_table = self.qualified_table.clone();
            let res = run_on_os_thread(move || -> Result<()> {
                let mut client = client.lock();
                client.batch_execute(&format!("REINDEX TABLE {qualified_table}"))?;
                Ok(())
            })
            .await;
            if let Err(e) = res {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "memory reindex: REINDEX failed; continuing with embedding backfill"
                );
            }
        }

        // 2) Backfill missing embedding vectors, but only when the vector stage
        //    is actually live (column + live embedder + vector-capable mode).
        if !self.vector_ready.load(Ordering::Acquire) {
            return Ok(0);
        }
        let embedder = self.current_embedder();
        if embedder.dimensions() == 0 {
            return Ok(0);
        }

        const BATCH: usize = 64;
        let client = self.client.get().clone();
        let qualified_table = self.qualified_table.clone();
        let mut updated = 0usize;

        loop {
            let batch = {
                let client = client.clone();
                let qualified_table = qualified_table.clone();
                run_on_os_thread(move || -> Result<Vec<(String, String)>> {
                    let mut client = client.lock();
                    let stmt = format!(
                        "SELECT id, content FROM {qualified_table} WHERE embedding IS NULL LIMIT $1"
                    );
                    #[allow(clippy::cast_possible_wrap)]
                    let rows = client.query(&stmt, &[&(BATCH as i64)])?;
                    Ok(rows
                        .iter()
                        .map(|r| {
                            let id: String = r.get("id");
                            let content: String = r.get("content");
                            (id, content)
                        })
                        .collect())
                })
                .await?
            };
            if batch.is_empty() {
                break;
            }

            let texts: Vec<&str> = batch.iter().map(|(_, c)| c.as_str()).collect();
            let vectors = match embedder.embed(&texts).await {
                Ok(v) => v,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                        "memory reindex: embedder failed mid-backfill; leaving remaining rows for a later pass"
                    );
                    break;
                }
            };
            if vectors.len() != batch.len() {
                // Provider returned a short batch; stop so we never spin on the
                // same unfillable rows. Partial progress is kept.
                for ((id, _), v) in batch.iter().zip(vectors.iter()) {
                    self.write_embedding(&client, &qualified_table, id, v)
                        .await?;
                    updated += 1;
                }
                break;
            }

            for ((id, _), v) in batch.iter().zip(vectors.iter()) {
                self.write_embedding(&client, &qualified_table, id, v)
                    .await?;
                updated += 1;
            }
        }

        Ok(updated)
    }

    fn refresh_embedder(
        &self,
        model_provider: &str,
        api_key: Option<&str>,
        model: &str,
        dimensions: usize,
    ) {
        // Rebuild from freshly-resolved settings and swap in place — mirrors
        // `SqliteMemory::refresh_embedder`. No provider state is cached twice.
        let embedder: Arc<dyn EmbeddingProvider> = Arc::from(
            embeddings::create_embedding_provider(model_provider, api_key, model, dimensions),
        );
        self.swap_embedder(embedder);
    }
    async fn ensure_agent_uuid(&self, alias: &str) -> Result<String> {
        let client = self.client.get().clone();
        let qualified_agents = self.qualified_agents.clone();
        let alias = alias.to_string();
        run_on_os_thread(move || -> Result<String> {
            let mut client = client.lock();
            let candidate = Uuid::new_v4().to_string();
            client.execute(
                &format!(
                    "INSERT INTO {qualified_agents} (id, alias, created_at)
                     VALUES ($1, $2, NOW())
                     ON CONFLICT (alias) DO NOTHING"
                ),
                &[&candidate, &alias],
            )?;
            let row: String = client
                .query_one(
                    &format!("SELECT id FROM {qualified_agents} WHERE alias = $1 LIMIT 1"),
                    &[&alias],
                )?
                .get(0);
            Ok(row)
        })
        .await
    }
}

impl ::zeroclaw_api::attribution::Attributable for PostgresMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::Postgres)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn postgres_test_url() -> String {
        std::env::var("ZEROCLAW_TEST_POSTGRES_URL")
            .expect("set ZEROCLAW_TEST_POSTGRES_URL to run ignored PostgreSQL tests")
    }

    /// Whether the test cluster can install the pgvector extension. Tests that
    /// assert vector-stage behavior skip (rather than fail) when this is false,
    /// mirroring how the backend degrades to keyword-only in production.
    fn pgvector_available(database_url: &str) -> bool {
        Client::connect(database_url, NoTls)
            .and_then(|mut client| {
                client
                    .query(
                        "SELECT 1 FROM pg_available_extensions WHERE name = 'vector'",
                        &[],
                    )
                    .map(|rows| !rows.is_empty())
            })
            .unwrap_or(false)
    }

    #[test]
    fn scoped_where_assigns_placeholders_in_push_order() {
        // No session, no agent, no window: empty fragment, index unchanged.
        assert_eq!(
            PostgresMemory::scoped_where(2, false, false, false, false),
            (String::new(), 2)
        );
        // session + since + until from $2 -> $2,$3,$4; next=5.
        let (frag, next) = PostgresMemory::scoped_where(2, true, false, true, true);
        assert_eq!(
            frag,
            " AND m.session_id = $2 AND m.created_at >= $3::TIMESTAMPTZ AND m.created_at <= $4::TIMESTAMPTZ"
        );
        assert_eq!(next, 5);
        // agent allowlist sits between session and the window.
        let (frag, next) = PostgresMemory::scoped_where(2, true, true, true, false);
        assert_eq!(
            frag,
            " AND m.session_id = $2 AND m.agent_id = ANY($3) AND m.created_at >= $4::TIMESTAMPTZ"
        );
        assert_eq!(next, 5);
    }

    #[test]
    fn valid_identifiers_pass_validation() {
        assert!(validate_identifier("public", "schema").is_ok());
        assert!(validate_identifier("_memories_01", "table").is_ok());
    }

    #[test]
    fn invalid_identifiers_are_rejected() {
        assert!(validate_identifier("", "schema").is_err());
        assert!(validate_identifier("1bad", "schema").is_err());
        assert!(validate_identifier("bad-name", "table").is_err());
    }

    #[test]
    fn parse_category_maps_known_and_custom_values() {
        assert_eq!(PostgresMemory::parse_category("core"), MemoryCategory::Core);
        assert_eq!(
            PostgresMemory::parse_category("daily"),
            MemoryCategory::Daily
        );
        assert_eq!(
            PostgresMemory::parse_category("conversation"),
            MemoryCategory::Conversation
        );
        assert_eq!(
            PostgresMemory::parse_category("custom_notes"),
            MemoryCategory::Custom("custom_notes".into())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drop_on_thread_drops_value_on_plain_os_thread() {
        let (tx, rx) = oneshot::channel::<bool>();

        struct DropGuard(Option<oneshot::Sender<bool>>);
        impl Drop for DropGuard {
            fn drop(&mut self) {
                // true  → dropped on a plain OS thread (no active Tokio runtime) ✓
                // false → dropped on a Tokio runtime thread ✗
                let on_plain_thread = tokio::runtime::Handle::try_current().is_err();
                let _ = self.0.take().unwrap().send(on_plain_thread);
            }
        }

        // Drop DropOnThread from inside the Tokio runtime — this is the
        // scenario that caused the nested-runtime panic in production.
        drop(DropOnThread::new(DropGuard(Some(tx))));

        let on_plain_thread = rx.await.expect("DropGuard did not fire");
        assert!(
            on_plain_thread,
            "DropOnThread must run Drop on a plain OS thread, not a Tokio runtime thread"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_does_not_panic_inside_tokio_runtime() {
        let outcome = std::panic::catch_unwind(|| {
            PostgresMemory::new(
                "test",
                "postgres://zeroclaw:password@127.0.0.1:1/zeroclaw",
                "public",
                "memories",
                Some(1),
                None,
                None,
            )
        });

        assert!(outcome.is_ok(), "PostgresMemory::new should not panic");
        assert!(
            outcome.unwrap().is_err(),
            "PostgresMemory::new should return a connect error for an unreachable endpoint"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_with_pgvector_enabled_does_not_panic_inside_tokio_runtime() {
        let outcome = std::panic::catch_unwind(|| {
            PostgresMemory::new(
                "test",
                "postgres://zeroclaw:password@127.0.0.1:1/zeroclaw",
                "public",
                "memories",
                Some(1),
                Some(true),
                Some(4),
            )
        });

        assert!(
            outcome.is_ok(),
            "PostgresMemory::new should not panic with pgvector enabled"
        );
        assert!(
            outcome.unwrap().is_err(),
            "PostgresMemory::new should return a connect error for an unreachable endpoint"
        );
    }

    struct TestSchema {
        admin: Client,
        name: String,
    }

    impl TestSchema {
        fn create(database_url: &str, purpose: &str) -> Self {
            let name = format!("zc_mem_{purpose}_{}", std::process::id());
            let mut admin = Client::connect(database_url, NoTls).unwrap();
            admin
                .batch_execute(&format!(
                    "DROP SCHEMA IF EXISTS {0} CASCADE; CREATE SCHEMA {0}",
                    quote_identifier(&name)
                ))
                .unwrap();
            Self { admin, name }
        }
    }

    impl Drop for TestSchema {
        fn drop(&mut self) {
            let _ = self.admin.batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {} CASCADE",
                quote_identifier(&self.name)
            ));
        }
    }

    /// Live regression for the nested-runtime panic in the pgvector-enabled
    /// construction path: `try_enable_pgvector` must run inside the
    /// `postgres-memory-init` OS thread, because the synchronous postgres
    /// client calls `block_on` internally and panics on a Tokio runtime
    /// thread. The pgvector extension does not need to be installed in the
    /// target database — an unavailable extension takes the graceful
    /// keyword-only fallback, while the pre-fix code panics before that
    /// fallback is reached.
    #[test]
    #[ignore = "requires ZEROCLAW_TEST_POSTGRES_URL"]
    fn postgres_new_with_pgvector_enabled_from_tokio_runtime_does_not_panic() {
        let database_url = std::env::var("ZEROCLAW_TEST_POSTGRES_URL")
            .expect("set ZEROCLAW_TEST_POSTGRES_URL to run ignored PostgreSQL tests");
        let schema = TestSchema::create(&database_url, "vec");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let memory = runtime.block_on(async {
            PostgresMemory::new(
                "test",
                &database_url,
                &schema.name,
                "memories",
                Some(5),
                Some(true),
                Some(4),
            )
        });

        let memory =
            memory.expect("pgvector-enabled construction from a Tokio runtime should succeed");
        drop(memory);
        drop(runtime);
    }

    #[test]
    #[ignore = "requires ZEROCLAW_TEST_POSTGRES_URL"]
    fn postgres_backend_round_trip_uses_isolated_schema() {
        let database_url = postgres_test_url();
        let schema = TestSchema::create(&database_url, "round_trip");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let memory = PostgresMemory::new(
                "ci",
                &database_url,
                &schema.name,
                "memories",
                Some(5),
                Some(false),
                None,
            )
            .unwrap();

            memory
                .store(
                    "ci_key",
                    "PostgreSQL CI value",
                    MemoryCategory::Core,
                    Some("ci_session"),
                )
                .await
                .unwrap();
            let stored = memory.get("ci_key").await.unwrap().unwrap();
            assert_eq!(stored.content, "PostgreSQL CI value");
            assert_eq!(memory.count().await.unwrap(), 1);
            assert!(memory.health_check().await);
            assert!(memory.forget("ci_key").await.unwrap());
            assert_eq!(memory.count().await.unwrap(), 0);
        });
    }

    #[test]
    fn rewrites_only_values_that_change_under_sanitization() {
        let distinct = vec![
            "slack_C123_1.2_user one".to_string(),
            "already_sanitized".to_string(),
            "whatsapp_123@g.us_alice".to_string(),
            "abc-DEF_123".to_string(),
        ];

        let rewrites = PostgresMemory::compute_session_id_rewrites(&distinct);
        assert_eq!(rewrites.len(), 2, "only the two raw forms need rewriting");

        let by_old: std::collections::HashMap<_, _> = rewrites.into_iter().collect();
        assert_eq!(
            by_old.get("slack_C123_1.2_user one").map(String::as_str),
            Some("slack_C123_1_2_user_one")
        );
        assert_eq!(
            by_old.get("whatsapp_123@g.us_alice").map(String::as_str),
            Some("whatsapp_123_g_us_alice")
        );
    }

    #[test]
    fn no_rewrites_when_all_values_already_sanitized() {
        let distinct = vec![
            "slack_C123_1_2_user_one".to_string(),
            "abc-DEF_123".to_string(),
            "".to_string(),
        ];
        let rewrites = PostgresMemory::compute_session_id_rewrites(&distinct);
        assert!(
            rewrites.is_empty(),
            "no UPDATE should be issued when every value is already sanitized"
        );
    }

    #[test]
    fn rewrite_plan_is_idempotent_when_reapplied() {
        let raw = "slack_C123_1.2_user one";
        let sanitized = sanitize_session_key(raw);

        let first = PostgresMemory::compute_session_id_rewrites(&[raw.to_string()]);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1, sanitized);

        let second = PostgresMemory::compute_session_id_rewrites(&[sanitized]);
        assert!(
            second.is_empty(),
            "re-running the plan over the rewritten value yields no further rewrite"
        );
    }

    // A deterministic stub embedder: a one-hot vector chosen by the first ASCII
    // letter of the text (mod dims). Lets the test control cosine similarity
    // independently of the keyword tokens, so the vector stage's contribution is
    // directly observable.
    #[derive(Clone, Copy)]
    struct FirstLetterEmbedding;

    #[async_trait::async_trait]
    impl crate::embeddings::EmbeddingProvider for FirstLetterEmbedding {
        fn name(&self) -> &str {
            "firstletter"
        }
        fn dimensions(&self) -> usize {
            4
        }
        async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let mut v = vec![0.0f32; 4];
                    let idx = t
                        .chars()
                        .find(|c| c.is_ascii_alphabetic())
                        .map(|c| ((c.to_ascii_lowercase() as u8 - b'a') % 4) as usize)
                        .unwrap_or(0);
                    v[idx] = 1.0;
                    v
                })
                .collect())
        }
    }

    /// Live end-to-end test of the embed-on-write + hybrid-recall feature:
    /// writes must populate the `embedding` column, and recall must use the
    /// vector stage to rank a document whose keywords do NOT match the query
    /// (so the keyword stage is empty and only cosine can rank it).
    #[test]
    #[ignore = "requires ZEROCLAW_TEST_POSTGRES_URL"]
    fn postgres_embed_on_write_and_vector_recall_end_to_end() {
        let database_url = postgres_test_url();
        if !pgvector_available(&database_url) {
            eprintln!(
                "skipping: pgvector extension is not installable on the test cluster; \
                 the vector stage it would exercise is unavailable by design"
            );
            return;
        }
        let schema = TestSchema::create(&database_url, "hybrid");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let (near_id, far_id) = runtime.block_on(async {
            let mem = PostgresMemory::with_embedder(
                "ci",
                &database_url,
                &schema.name,
                "memories",
                Some(5),
                Some(true), // pgvector
                Some(4),    // dims
                Arc::new(FirstLetterEmbedding),
                0.7,
                0.3,
                zeroclaw_config::schema::SearchMode::Hybrid,
            )
            .unwrap();

            // "banana ..." → first letter 'b' → one-hot idx1.
            // "cherry ..." → first letter 'c' → one-hot idx2.
            mem.store("k_near", "banana xylophone", MemoryCategory::Core, None)
                .await
                .unwrap();
            mem.store("k_far", "cherry xylophone", MemoryCategory::Core, None)
                .await
                .unwrap();

            // Query "burrito" → first letter 'b' (vector matches k_near), token
            // {burrito} matches NEITHER doc's keywords, so only cosine can rank.
            let hits = mem.recall("burrito", 10, None, None, None).await.unwrap();
            assert!(
                !hits.is_empty(),
                "hybrid recall must surface the vector-nearest doc even when the keyword stage is empty"
            );
            assert_eq!(
                hits[0].key, "k_near",
                "vector stage must rank the cosine-nearest doc first, got {:?}",
                hits.iter().map(|h| (&h.key, h.score)).collect::<Vec<_>>()
            );
            (hits[0].id.clone(), hits[1].id.clone())
        });

        // Readback: both rows must have a non-null embedding written on store.
        let mut admin = Client::connect(&database_url, NoTls).unwrap();
        let nn: i64 = admin
            .query_one(
                &format!(
                    "SELECT COUNT(*) FROM {}.memories WHERE embedding IS NOT NULL",
                    quote_identifier(&schema.name)
                ),
                &[],
            )
            .unwrap()
            .get(0);
        assert_eq!(nn, 2, "embed-on-write must populate the embedding column");

        // And the stored vector must actually be the expected one-hot for 'b'.
        let emb: String = admin
            .query_one(
                &format!(
                    "SELECT embedding::text FROM {}.memories WHERE key = 'k_near'",
                    quote_identifier(&schema.name)
                ),
                &[],
            )
            .unwrap()
            .get(0);
        assert!(
            emb.contains("[0,1,0,0]"),
            "stored embedding for 'banana' should be the 'b' one-hot [0,1,0,0], got {emb}"
        );
        assert!(near_id != far_id);
    }
}
