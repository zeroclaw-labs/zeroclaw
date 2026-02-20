//! PostgreSQL + pgvector storage for the RAG pipeline.
//!
//! Manages `rag_sources` and `rag_chunks` tables.  All vector data is passed as
//! pgvector literal strings (`[x1,x2,…]`) and cast to the `vector` type inside
//! SQL, so no extra postgres feature flags are required beyond what the project
//! already enables.
//!
//! All blocking postgres calls are wrapped in `tokio::task::spawn_blocking` to
//! keep the async executor healthy, matching the pattern used by
//! [`crate::memory::postgres::PostgresMemory`].

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use postgres::{Client, NoTls};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Cap on PostgreSQL connect timeout (seconds).
const CONNECT_TIMEOUT_CAP_SECS: u64 = 300;

/// Serialise a `Vec<f32>` to the pgvector literal format: `[x1,x2,…]`.
fn vec_to_pgvector_literal(v: &[f32]) -> String {
    let inner: Vec<String> = v.iter().map(|f| f.to_string()).collect();
    format!("[{}]", inner.join(","))
}

// ── PgVectorStore ──────────────────────────────────────────────────────────────

/// Shared handle to the underlying postgres connection.
type SharedClient = Arc<Mutex<Client>>;

/// PostgreSQL-backed vector store for RAG sources and chunks.
pub struct PgVectorStore {
    client: SharedClient,
}

impl PgVectorStore {
    /// Connect to PostgreSQL and ensure the RAG schema is in place.
    ///
    /// `connect_timeout_secs` caps at [`CONNECT_TIMEOUT_CAP_SECS`] to prevent
    /// unreasonable waits.
    pub fn new(db_url: &str, connect_timeout_secs: Option<u64>) -> Result<Self> {
        let mut config: postgres::Config = db_url
            .parse()
            .context("invalid PostgreSQL connection URL for RAG store")?;

        if let Some(t) = connect_timeout_secs {
            config.connect_timeout(Duration::from_secs(t.min(CONNECT_TIMEOUT_CAP_SECS)));
        }

        let do_connect = || -> Result<Client> {
            let mut client = config
                .connect(NoTls)
                .context("failed to connect to PostgreSQL RAG backend")?;
            Self::init_schema(&mut client)?;
            Ok(client)
        };

        let client = if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(do_connect)
        } else {
            do_connect()
        }?;

        Ok(Self {
            client: Arc::new(Mutex::new(client)),
        })
    }

    /// Create the `rag_sources` and `rag_chunks` tables and their indexes.
    ///
    /// Idempotent — safe to run on every startup.
    fn init_schema(client: &mut Client) -> Result<()> {
        // Enable pgvector extension.  The existing PostgresMemory does the same
        // at its own init; doing it here too is harmless (IF NOT EXISTS).
        client
            .batch_execute("CREATE EXTENSION IF NOT EXISTS vector;")
            .context(
                "failed to enable pgvector extension; \
                 ensure the extension is installed on the PostgreSQL server",
            )?;

        client
            .batch_execute(
                "
                CREATE TABLE IF NOT EXISTS rag_sources (
                    id TEXT PRIMARY KEY,
                    source_type TEXT NOT NULL,
                    source_url TEXT,
                    title TEXT,
                    content_hash TEXT,
                    crawled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    last_updated TIMESTAMPTZ,
                    metadata TEXT NOT NULL DEFAULT '{}',
                    active BOOLEAN NOT NULL DEFAULT TRUE
                );

                CREATE INDEX IF NOT EXISTS idx_rag_sources_crawled_at
                    ON rag_sources(crawled_at);

                CREATE INDEX IF NOT EXISTS idx_rag_sources_source_url
                    ON rag_sources(source_url);

                CREATE TABLE IF NOT EXISTS rag_chunks (
                    id TEXT PRIMARY KEY,
                    source_id TEXT NOT NULL REFERENCES rag_sources(id) ON DELETE CASCADE,
                    chunk_index INTEGER NOT NULL,
                    content TEXT NOT NULL,
                    heading_context TEXT,
                    embedding vector(1024) NOT NULL,
                    token_count INTEGER,
                    metadata TEXT NOT NULL DEFAULT '{}',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    UNIQUE(source_id, chunk_index)
                );

                CREATE INDEX IF NOT EXISTS idx_rag_chunks_source_id
                    ON rag_chunks(source_id);

                CREATE INDEX IF NOT EXISTS idx_rag_sources_active
                    ON rag_sources(active);
                ",
            )
            .context("failed to create RAG schema")?;

        // HNSW index requires pgvector >= 0.5.  Use a separate statement so a
        // failure here gives a clear diagnostic rather than rolling back the
        // whole schema init.
        if let Err(e) = client.batch_execute(
            "CREATE INDEX IF NOT EXISTS idx_rag_chunks_embedding
                 ON rag_chunks USING hnsw (embedding vector_cosine_ops)
                 WITH (m = 16, ef_construction = 200);",
        ) {
            tracing::warn!(
                "Could not create HNSW index on rag_chunks.embedding (pgvector >= 0.5 required): {e}"
            );
        }

        Ok(())
    }

    // ── Source operations ──────────────────────────────────────────────────────

    /// Look up an existing source by URL.  Returns `None` if not found.
    pub async fn find_source_by_url(&self, url: &str) -> Result<Option<RagSource>> {
        let client = Arc::clone(&self.client);
        let url = url.to_string();

        tokio::task::spawn_blocking(move || -> Result<Option<RagSource>> {
            let mut c = client.lock();
            let row = c
                .query_opt(
                    "SELECT id, source_type, source_url, title, content_hash, crawled_at, last_updated, active
                     FROM rag_sources
                     WHERE source_url = $1
                     LIMIT 1",
                    &[&url],
                )
                .context("failed to query rag_sources by URL")?;

            Ok(row.as_ref().map(row_to_rag_source))
        })
        .await?
    }

    /// Insert a new source row.  Returns the generated `id`.
    pub async fn insert_source(&self, req: &InsertSourceRequest) -> Result<String> {
        let client = Arc::clone(&self.client);
        let id = Uuid::new_v4().to_string();
        let source_type = req.source_type.clone();
        let source_url = req.source_url.clone();
        let title = req.title.clone();
        let content_hash = req.content_hash.clone();
        let now: DateTime<Utc> = Utc::now();
        let id_out = id.clone();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut c = client.lock();
            c.execute(
                "INSERT INTO rag_sources
                     (id, source_type, source_url, title, content_hash, crawled_at, active)
                 VALUES ($1, $2, $3, $4, $5, $6, TRUE)",
                &[&id, &source_type, &source_url, &title, &content_hash, &now],
            )
            .context("failed to insert into rag_sources")?;
            Ok(())
        })
        .await??;

        Ok(id_out)
    }

    /// Update an existing source's hash and `last_updated` timestamp.
    pub async fn update_source_hash(&self, source_id: &str, content_hash: &str) -> Result<()> {
        let client = Arc::clone(&self.client);
        let source_id = source_id.to_string();
        let content_hash = content_hash.to_string();
        let now: DateTime<Utc> = Utc::now();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut c = client.lock();
            c.execute(
                "UPDATE rag_sources SET content_hash = $1, last_updated = $2 WHERE id = $3",
                &[&content_hash, &now, &source_id],
            )
            .context("failed to update rag_source content_hash")?;
            Ok(())
        })
        .await?
    }

    // ── Chunk operations ───────────────────────────────────────────────────────

    /// Delete all chunks for a source (used before re-ingesting updated content).
    pub async fn delete_chunks_for_source(&self, source_id: &str) -> Result<()> {
        let client = Arc::clone(&self.client);
        let source_id = source_id.to_string();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut c = client.lock();
            c.execute("DELETE FROM rag_chunks WHERE source_id = $1", &[&source_id])
                .context("failed to delete existing chunks for source")?;
            Ok(())
        })
        .await?
    }

    /// Insert a batch of chunks for a source within a single transaction.
    ///
    /// `embeddings` must be the same length as `chunks`.
    pub async fn insert_chunks(&self, source_id: &str, chunks: &[ChunkRow]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let client = Arc::clone(&self.client);
        let source_id = source_id.to_string();
        let chunks = chunks.to_vec();
        let now: DateTime<Utc> = Utc::now();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut c = client.lock();
            let mut tx = c
                .transaction()
                .context("failed to start chunk insert transaction")?;

            for row in &chunks {
                let id = Uuid::new_v4().to_string();
                let embedding_literal = vec_to_pgvector_literal(&row.embedding);

                let chunk_index_i32 =
                    i32::try_from(row.chunk_index).context("chunk_index overflows i32")?;
                let token_count_i32: Option<i32> = row
                    .token_count
                    .map(i32::try_from)
                    .transpose()
                    .context("token_count overflows i32")?;

                tx.execute(
                    "INSERT INTO rag_chunks
                         (id, source_id, chunk_index, content, heading_context,
                          embedding, token_count, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6::vector, $7, $8)",
                    &[
                        &id,
                        &source_id,
                        &chunk_index_i32,
                        &row.content,
                        &row.heading_context,
                        &embedding_literal,
                        &token_count_i32,
                        &now,
                    ],
                )
                .context("failed to insert chunk")?;
            }

            tx.commit()
                .context("failed to commit chunk insert transaction")?;
            Ok(())
        })
        .await?
    }

    // ── Retrieval ──────────────────────────────────────────────────────────────

    /// Cosine-similarity search over all active chunks.
    ///
    /// Returns up to `top_k` results ordered by descending similarity.
    /// Chunks with similarity below `min_similarity` are excluded.
    pub async fn similarity_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
        min_similarity: f64,
    ) -> Result<Vec<SimilarityRow>> {
        let client = Arc::clone(&self.client);
        let embedding_literal = vec_to_pgvector_literal(query_embedding);
        #[allow(clippy::cast_possible_wrap)]
        let top_k_i64 = top_k as i64;
        let min_similarity_f64 = min_similarity;

        tokio::task::spawn_blocking(move || -> Result<Vec<SimilarityRow>> {
            let mut c = client.lock();

            let rows = c
                .query(
                    "SELECT c.content,
                            c.heading_context,
                            s.source_url,
                            s.title,
                            1.0 - (c.embedding <=> $1::vector) AS similarity
                     FROM rag_chunks c
                     JOIN rag_sources s ON c.source_id = s.id
                     WHERE s.active = TRUE
                       AND 1.0 - (c.embedding <=> $1::vector) >= $2
                     ORDER BY c.embedding <=> $1::vector
                     LIMIT $3",
                    &[&embedding_literal, &min_similarity_f64, &top_k_i64],
                )
                .context("failed to execute similarity search")?;

            let results = rows
                .iter()
                .map(|row| SimilarityRow {
                    content: row.get(0),
                    heading_context: row.get(1),
                    source_url: row.get(2),
                    source_title: row.get(3),
                    similarity_score: row.get(4),
                })
                .collect();

            Ok(results)
        })
        .await?
    }
}

// ── Data types ─────────────────────────────────────────────────────────────────

/// A row from `rag_sources` (subset of columns used by the ingest pipeline).
#[derive(Debug, Clone)]
pub struct RagSource {
    pub id: String,
    pub source_type: String,
    pub source_url: Option<String>,
    pub title: Option<String>,
    pub content_hash: Option<String>,
    pub crawled_at: DateTime<Utc>,
    pub last_updated: Option<DateTime<Utc>>,
    pub active: bool,
}

/// Parameters for inserting a new source.
#[derive(Debug, Clone)]
pub struct InsertSourceRequest {
    pub source_type: String,
    pub source_url: Option<String>,
    pub title: Option<String>,
    /// SHA-256 (hex) of the raw content, used to detect unchanged sources.
    pub content_hash: Option<String>,
}

/// A single chunk to be inserted into `rag_chunks`.
#[derive(Debug, Clone)]
pub struct ChunkRow {
    pub chunk_index: usize,
    pub content: String,
    pub heading_context: Option<String>,
    pub embedding: Vec<f32>,
    pub token_count: Option<usize>,
}

/// A row returned by [`PgVectorStore::similarity_search`].
#[derive(Debug, Clone)]
pub struct SimilarityRow {
    pub content: String,
    pub heading_context: Option<String>,
    pub source_url: Option<String>,
    pub source_title: Option<String>,
    pub similarity_score: f64,
}

// ── Row helpers ────────────────────────────────────────────────────────────────

fn row_to_rag_source(row: &postgres::Row) -> RagSource {
    RagSource {
        id: row.get(0),
        source_type: row.get(1),
        source_url: row.get(2),
        title: row.get(3),
        content_hash: row.get(4),
        crawled_at: row.get(5),
        last_updated: row.get(6),
        active: row.get(7),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_to_pgvector_literal_formats_correctly() {
        let v = vec![1.0f32, 2.5, -0.5];
        let s = vec_to_pgvector_literal(&v);
        assert!(s.starts_with('['));
        assert!(s.ends_with(']'));
        assert!(s.contains('1'));
        assert!(s.contains("2.5"));
        assert!(s.contains("-0.5"));
    }

    #[test]
    fn vec_to_pgvector_literal_empty() {
        let s = vec_to_pgvector_literal(&[]);
        assert_eq!(s, "[]");
    }

    #[test]
    fn vec_to_pgvector_literal_single() {
        let s = vec_to_pgvector_literal(&[0.123_456_79]);
        assert!(s.starts_with('['));
        assert!(s.ends_with(']'));
    }
}
