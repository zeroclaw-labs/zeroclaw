use super::embeddings::EmbeddingProvider;
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use super::vector;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pgvector::Vector;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};
use uuid::Uuid;

/// PostgreSQL + pgvector memory backend with hybrid semantic + keyword search.
///
/// Uses tokio-postgres (async native) for non-blocking database operations.
/// Stores memories with vector embeddings and full-text search indexes.
/// Recall uses weighted hybrid fusion: vector similarity + keyword matching.
pub struct PgVectorMemory {
    client: Arc<Mutex<Client>>,
    qualified_table: String,
    embedding: Arc<dyn EmbeddingProvider>,
    vector_weight: f32,
    keyword_weight: f32,
}

impl PgVectorMemory {
    pub async fn new(
        url: &str,
        schema: &str,
        table: &str,
        embedding: Arc<dyn EmbeddingProvider>,
        vector_weight: f32,
        keyword_weight: f32,
    ) -> Result<Self> {
        validate_ident(schema, "schema")?;
        validate_ident(table, "table")?;

        let schema_q = quote(schema);
        let table_q = quote(table);
        let qualified = format!("{schema_q}.{table_q}");

        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .context("pgvector connection failed")?;

        // Spawn the connection task (required by tokio-postgres)
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("pgvector connection error: {e}");
            }
        });

        // Verify pgvector extension
        let row = client
            .query_opt("SELECT 1 FROM pg_extension WHERE extname = 'vector'", &[])
            .await?;
        if row.is_none() {
            anyhow::bail!("pgvector extension not installed; run CREATE EXTENSION vector");
        }

        // Init schema (idempotent)
        client
            .batch_execute(&format!(
                "
                CREATE SCHEMA IF NOT EXISTS {schema_q};

                CREATE TABLE IF NOT EXISTS {qualified} (
                    id TEXT PRIMARY KEY,
                    key TEXT UNIQUE NOT NULL,
                    content TEXT NOT NULL,
                    category TEXT NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    session_id TEXT,
                    content_embedding vector(768),
                    content_tsvector tsvector GENERATED ALWAYS AS (
                        to_tsvector('english', content)
                    ) STORED
                );

                CREATE INDEX IF NOT EXISTS idx_pgv_embedding
                    ON {qualified} USING hnsw (content_embedding vector_cosine_ops)
                    WITH (m = 16, ef_construction = 64);
                CREATE INDEX IF NOT EXISTS idx_pgv_fts
                    ON {qualified} USING gin (content_tsvector);
                CREATE INDEX IF NOT EXISTS idx_pgv_category
                    ON {qualified}(category);
                CREATE INDEX IF NOT EXISTS idx_pgv_session
                    ON {qualified}(session_id);
                CREATE INDEX IF NOT EXISTS idx_pgv_updated
                    ON {qualified}(updated_at DESC);
                "
            ))
            .await?;

        Ok(Self {
            client: Arc::new(Mutex::new(client)),
            qualified_table: qualified,
            embedding,
            vector_weight,
            keyword_weight,
        })
    }
}

fn category_str(cat: &MemoryCategory) -> String {
    match cat {
        MemoryCategory::Core => "core".into(),
        MemoryCategory::Daily => "daily".into(),
        MemoryCategory::Conversation => "conversation".into(),
        MemoryCategory::Custom(s) => s.clone(),
    }
}

fn parse_category(s: &str) -> MemoryCategory {
    match s {
        "core" => MemoryCategory::Core,
        "daily" => MemoryCategory::Daily,
        "conversation" => MemoryCategory::Conversation,
        other => MemoryCategory::Custom(other.into()),
    }
}

fn validate_ident(val: &str, field: &str) -> Result<()> {
    if val.is_empty() {
        anyhow::bail!("{field} must not be empty");
    }
    let mut chars = val.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        anyhow::bail!("{field} must start with letter or underscore; got '{val}'");
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        anyhow::bail!("{field} has invalid characters; got '{val}'");
    }
    Ok(())
}

fn quote(val: &str) -> String {
    format!("\"{val}\"")
}

#[async_trait]
impl Memory for PgVectorMemory {
    fn name(&self) -> &str {
        "pgvector"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let emb = self.embedding.embed_one(content).await?;
        let vec = Vector::from(emb);
        let now = Utc::now();
        let id = Uuid::new_v4().to_string();
        let cat = category_str(&category);

        let client = self.client.lock().await;
        client
            .execute(
                &format!(
                    "INSERT INTO {}
                        (id, key, content, category, created_at, updated_at, session_id, content_embedding)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                     ON CONFLICT (key) DO UPDATE SET
                        content = EXCLUDED.content,
                        category = EXCLUDED.category,
                        updated_at = EXCLUDED.updated_at,
                        session_id = EXCLUDED.session_id,
                        content_embedding = EXCLUDED.content_embedding",
                    self.qualified_table
                ),
                &[&id, &key, &content, &cat, &now, &now, &session_id, &vec],
            )
            .await?;

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(vec![]);
        }

        let emb = self.embedding.embed_one(query).await?;
        let vec = Vector::from(emb);
        let table = &self.qualified_table;

        #[allow(clippy::cast_possible_wrap)]
        let fetch = (limit * 3) as i64;

        let client = self.client.lock().await;

        // Vector search
        let vector_rows = client
            .query(
                &format!(
                    "SELECT id, 1.0 - (content_embedding <=> $1) AS score
                     FROM {table}
                     WHERE content_embedding IS NOT NULL
                       AND ($2::TEXT IS NULL OR session_id = $2)
                     ORDER BY content_embedding <=> $1
                     LIMIT $3"
                ),
                &[&vec, &session_id, &fetch],
            )
            .await?;

        let vector_results: Vec<(String, f32)> = vector_rows
            .iter()
            .map(|r| {
                let id: String = r.get(0);
                let score: f64 = r.get(1);
                (id, score as f32)
            })
            .collect();

        // Keyword search
        let kw_rows = client
            .query(
                &format!(
                    "SELECT id, ts_rank(content_tsvector, plainto_tsquery('english', $1)) AS score
                     FROM {table}
                     WHERE content_tsvector @@ plainto_tsquery('english', $1)
                       AND ($2::TEXT IS NULL OR session_id = $2)
                     ORDER BY score DESC
                     LIMIT $3"
                ),
                &[&query, &session_id, &fetch],
            )
            .await?;

        let keyword_results: Vec<(String, f32)> = kw_rows
            .iter()
            .map(|r| {
                let id: String = r.get(0);
                let score: f32 = r.get(1);
                (id, score)
            })
            .collect();

        // Hybrid merge
        let merged = vector::hybrid_merge(
            &vector_results,
            &keyword_results,
            self.vector_weight,
            self.keyword_weight,
            limit,
        );

        if merged.is_empty() {
            return Ok(vec![]);
        }

        // Fetch full entries
        let ids: Vec<String> = merged.iter().map(|r| r.id.clone()).collect();
        let scores: std::collections::HashMap<String, f32> = merged
            .iter()
            .map(|r| (r.id.clone(), r.final_score))
            .collect();

        let placeholders: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("${}", i + 1))
            .collect();
        let in_clause = placeholders.join(", ");

        let fetch_sql = format!(
            "SELECT id, key, content, category, created_at, session_id
             FROM {table}
             WHERE id IN ({in_clause})"
        );

        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = ids
            .iter()
            .map(|s| s as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let rows = client.query(&fetch_sql, &params).await?;

        let mut entries: Vec<MemoryEntry> = rows
            .iter()
            .map(|r| {
                let ts: DateTime<Utc> = r.get(4);
                let id: String = r.get(0);
                let score = scores.get(&id).copied();
                MemoryEntry {
                    id: id.clone(),
                    key: r.get(1),
                    content: r.get(2),
                    category: parse_category(&r.get::<_, String>(3)),
                    timestamp: ts.to_rfc3339(),
                    session_id: r.get(5),
                    score: score.map(f64::from),
                }
            })
            .collect();

        entries.sort_by(|a, b| {
            b.score
                .unwrap_or(0.0)
                .partial_cmp(&a.score.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(entries)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let client = self.client.lock().await;
        let row = client
            .query_opt(
                &format!(
                    "SELECT id, key, content, category, created_at, session_id
                     FROM {} WHERE key = $1 LIMIT 1",
                    self.qualified_table
                ),
                &[&key],
            )
            .await?;

        match row {
            Some(r) => {
                let ts: DateTime<Utc> = r.get(4);
                Ok(Some(MemoryEntry {
                    id: r.get(0),
                    key: r.get(1),
                    content: r.get(2),
                    category: parse_category(&r.get::<_, String>(3)),
                    timestamp: ts.to_rfc3339(),
                    session_id: r.get(5),
                    score: None,
                }))
            }
            None => Ok(None),
        }
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let cat = category.map(category_str);
        let cat_ref = cat.as_deref();
        let client = self.client.lock().await;
        let rows = client
            .query(
                &format!(
                    "SELECT id, key, content, category, created_at, session_id
                     FROM {}
                     WHERE ($1::TEXT IS NULL OR category = $1)
                       AND ($2::TEXT IS NULL OR session_id = $2)
                     ORDER BY updated_at DESC",
                    self.qualified_table
                ),
                &[&cat_ref, &session_id],
            )
            .await?;

        rows.iter()
            .map(|r| {
                let ts: DateTime<Utc> = r.get(4);
                Ok(MemoryEntry {
                    id: r.get(0),
                    key: r.get(1),
                    content: r.get(2),
                    category: parse_category(&r.get::<_, String>(3)),
                    timestamp: ts.to_rfc3339(),
                    session_id: r.get(5),
                    score: None,
                })
            })
            .collect()
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let client = self.client.lock().await;
        let n = client
            .execute(
                &format!("DELETE FROM {} WHERE key = $1", self.qualified_table),
                &[&key],
            )
            .await?;
        Ok(n > 0)
    }

    async fn count(&self) -> Result<usize> {
        let client = self.client.lock().await;
        let row = client
            .query_one(
                &format!("SELECT COUNT(*) FROM {}", self.qualified_table),
                &[],
            )
            .await?;
        let n: i64 = row.get(0);
        usize::try_from(n).context("negative count")
    }

    async fn health_check(&self) -> bool {
        let client = self.client.lock().await;
        client
            .query_opt("SELECT 1 FROM pg_extension WHERE extname = 'vector'", &[])
            .await
            .map(|r| r.is_some())
            .unwrap_or(false)
    }
}

impl PgVectorMemory {
    /// Generate embeddings for memories that lack them.
    ///
    /// Returns the number of memories that were updated. Use after bulk
    /// importing data without embeddings, or when switching embedding models.
    pub async fn reindex(&self, batch: usize) -> Result<usize> {
        let client = self.client.lock().await;
        let rows = client
            .query(
                &format!(
                    "SELECT key, content FROM {}
                     WHERE content_embedding IS NULL
                     ORDER BY updated_at DESC
                     LIMIT $1",
                    self.qualified_table
                ),
                &[&(batch as i64)],
            )
            .await?;

        if rows.is_empty() {
            return Ok(0);
        }

        let texts: Vec<&str> = rows.iter().map(|r| r.get::<_, &str>(1)).collect();
        let embeddings = self.embedding.embed(&texts).await?;

        let mut updated = 0;
        for (row, emb) in rows.iter().zip(embeddings.into_iter()) {
            let key: &str = row.get(0);
            let vec = Vector::from(emb);
            let n = client
                .execute(
                    &format!(
                        "UPDATE {} SET content_embedding = $1 WHERE key = $2",
                        self.qualified_table
                    ),
                    &[&vec, &key],
                )
                .await?;
            updated += n as usize;
        }

        Ok(updated)
    }

    /// Return the number of memories that lack embeddings.
    pub async fn unindexed_count(&self) -> Result<usize> {
        let client = self.client.lock().await;
        let row = client
            .query_one(
                &format!(
                    "SELECT COUNT(*) FROM {} WHERE content_embedding IS NULL",
                    self.qualified_table
                ),
                &[],
            )
            .await?;
        let n: i64 = row.get(0);
        usize::try_from(n).context("negative count")
    }

    /// Return basic statistics about the memory store.
    pub async fn stats(&self) -> Result<PgVectorStats> {
        let client = self.client.lock().await;
        let total: i64 = client
            .query_one(
                &format!("SELECT COUNT(*) FROM {}", self.qualified_table),
                &[],
            )
            .await?
            .get(0);
        let indexed: i64 = client
            .query_one(
                &format!(
                    "SELECT COUNT(*) FROM {} WHERE content_embedding IS NOT NULL",
                    self.qualified_table
                ),
                &[],
            )
            .await?
            .get(0);

        let by_category = client
            .query(
                &format!(
                    "SELECT category, COUNT(*) FROM {} GROUP BY category ORDER BY COUNT(*) DESC",
                    self.qualified_table
                ),
                &[],
            )
            .await?;

        let categories: Vec<(String, usize)> = by_category
            .iter()
            .map(|r| {
                let cat: String = r.get(0);
                let cnt: i64 = r.get(1);
                (cat, cnt as usize)
            })
            .collect();

        Ok(PgVectorStats {
            total: total as usize,
            indexed: indexed as usize,
            unindexed: (total - indexed) as usize,
            categories,
        })
    }
}

/// Statistics for the pgvector memory store.
#[derive(Debug)]
pub struct PgVectorStats {
    pub total: usize,
    pub indexed: usize,
    pub unindexed: usize,
    pub categories: Vec<(String, usize)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_identifiers_pass() {
        assert!(validate_ident("zeroclaw_memory", "schema").is_ok());
        assert!(validate_ident("_test_01", "table").is_ok());
    }

    #[test]
    fn invalid_identifiers_rejected() {
        assert!(validate_ident("", "schema").is_err());
        assert!(validate_ident("1bad", "schema").is_err());
        assert!(validate_ident("bad-name", "table").is_err());
    }

    #[test]
    fn category_roundtrip() {
        assert_eq!(parse_category("core"), MemoryCategory::Core);
        assert_eq!(parse_category("daily"), MemoryCategory::Daily);
        assert_eq!(
            parse_category("custom_x"),
            MemoryCategory::Custom("custom_x".into())
        );
    }
}
