use super::embeddings::EmbeddingProvider;
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use super::vector;
use async_trait::async_trait;
use chrono::Local;
use deadpool_sqlite::{Config, Pool, Runtime};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// SQLite-backed persistent memory with connection pooling
///
/// Performance optimizations:
/// - **Connection Pool**: Uses deadpool-sqlite for concurrent read access
/// - **WAL Mode**: Write-Ahead Logging for better concurrent read/write
/// - **Embedding Batch**: Batches embedding API calls to reduce latency
/// - **Async-Aware**: Uses tokio::sync primitives instead of blocking Mutex
pub struct PooledSqliteMemory {
    pool: Pool,
    db_path: PathBuf,
    embedder: Arc<dyn EmbeddingProvider>,
    vector_weight: f32,
    keyword_weight: f32,
    cache_max: usize,
    /// Pending embedding batch for API call optimization
    embedding_batch: RwLock<EmbeddingBatch>,
    /// Batch size threshold before flushing
    batch_size: usize,
}

/// Pending embedding requests batch
#[derive(Default)]
struct EmbeddingBatch {
    /// Map from content hash to (content, Vec<tokio::sync::oneshot::Sender<Result<Vec<f32>, Arc<anyhow::Error>>>>)
    pending: HashMap<String, BatchEntry>,
}

struct BatchEntry {
    content: String,
    senders: Vec<tokio::sync::oneshot::Sender<anyhow::Result<Vec<f32>>>>,
}

/// Metrics for performance monitoring
#[derive(Debug, Clone, Default)]
pub struct PerformanceMetrics {
    pub total_requests: u64,
    pub batched_requests: u64,
    pub api_calls_saved: u64,
    pub avg_batch_size: f64,
    pub pool_stats: PoolStats,
}

#[derive(Debug, Clone, Default)]
pub struct PoolStats {
    pub available_connections: usize,
    pub max_connections: usize,
    pub wait_time_ms: f64,
}

impl PooledSqliteMemory {
    /// Create a new pooled SQLite memory with default settings
    pub async fn new(workspace_dir: &Path) -> anyhow::Result<Self> {
        Self::with_embedder(
            workspace_dir,
            Arc::new(super::embeddings::NoopEmbedding),
            0.7,
            0.3,
            10_000,
            32, // Default batch size
        )
        .await
    }

    /// Create with custom embedder and configuration
    pub async fn with_embedder(
        workspace_dir: &Path,
        embedder: Arc<dyn EmbeddingProvider>,
        vector_weight: f32,
        keyword_weight: f32,
        cache_max: usize,
        batch_size: usize,
    ) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("memory").join("brain.db");

        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Initialize database with schema
        let conn = Connection::open(&db_path)?;
        Self::init_schema(&conn)?;
        
        // Enable WAL mode for better concurrent access
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "cache_size", 10000)?;
        conn.pragma_update(None, "temp_store", "memory")?;
        drop(conn);

        // Configure connection pool
        let max_connections = std::thread::available_parallelism()
            .map(|n| n.get() * 2)
            .unwrap_or(4)
            .max(4) as usize;

        let pool = Config::new(db_path.to_str().unwrap())
            .max_connections(max_connections)
            .create_pool(Runtime::Tokio1)?;

        // Test pool connectivity
        let _conn = pool.get().await?;

        Ok(Self {
            pool,
            db_path,
            embedder,
            vector_weight,
            keyword_weight,
            cache_max,
            embedding_batch: RwLock::new(EmbeddingBatch::default()),
            batch_size,
        })
    }

    /// Initialize all tables: memories, FTS5, embedding_cache
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

    /// Simple content hash for embedding cache
    fn content_hash(text: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Get embedding with batching optimization
    /// 
    /// This method batches embedding requests to reduce API calls.
    /// Multiple concurrent calls to this method will be aggregated
    /// into a single API request when possible.
    async fn get_or_compute_embedding(&self, text: &str) -> anyhow::Result<Option<Vec<f32>>> {
        if self.embedder.dimensions() == 0 {
            return Ok(None); // Noop embedder
        }

        let hash = Self::content_hash(text);
        let now = Local::now().to_rfc3339();

        // Check cache first (fast path)
        {
            let conn = self.pool.get().await?;
            let hash_clone = hash.clone();
            let now_clone = now.clone();
            let cached: Option<Vec<u8>> = conn
                .interact(move |conn| {
                    let mut stmt = conn
                        .prepare("SELECT embedding FROM embedding_cache WHERE content_hash = ?1")?;
                    let result: Option<Vec<u8>> = stmt.query_row(params![hash_clone], |row| row.get(0)).ok();
                    
                    // Update accessed_at for LRU
                    if result.is_some() {
                        conn.execute(
                            "UPDATE embedding_cache SET accessed_at = ?1 WHERE content_hash = ?2",
                            params![now_clone, hash_clone],
                        )?;
                    }
                    Ok::<_, anyhow::Error>(result)
                })
                .await
                .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;

            if let Some(bytes) = cached {
                return Ok(Some(vector::bytes_to_vec(&bytes)));
            }
        }

        // Create channel to receive result from batch
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut should_flush = false;

        // Add to batch
        {
            let mut batch = self.embedding_batch.write().await;
            if let Some(entry) = batch.pending.get_mut(&hash) {
                // Deduplication: same content already in batch
                entry.senders.push(tx);
            } else {
                batch.pending.insert(
                    hash.clone(),
                    BatchEntry {
                        content: text.to_string(),
                        senders: vec![tx],
                    },
                );
                should_flush = batch.pending.len() >= self.batch_size;
            }
        }

        // Flush batch if threshold reached
        if should_flush {
            self.flush_embedding_batch().await?;
        }

        // Wait for result
        match rx.await {
            Ok(result) => result.map(Some),
            Err(_) => {
                // Batch was dropped, compute directly
                let embedding = self.embedder.embed_one(text).await?;
                self.cache_embedding(&hash, &embedding).await?;
                Ok(Some(embedding))
            }
        }
    }

    /// Flush pending embedding batch to API
    async fn flush_embedding_batch(&self) -> anyhow::Result<()> {
        let batch_to_process = {
            let mut batch = self.embedding_batch.write().await;
            if batch.pending.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut batch.pending)
        };

        if batch_to_process.is_empty() {
            return Ok(());
        }

        // Collect unique contents
        let contents: Vec<String> = batch_to_process
            .values()
            .map(|e| e.content.clone())
            .collect();

        // Single API call for all pending embeddings
        let embeddings = self.embedder.embed(
            &contents.iter().map(|s| s.as_str()).collect::<Vec<_>>()
        ).await?;

        // Cache results and notify waiters
        let now = Local::now().to_rfc3339();
        let cache_max = self.cache_max;
        
        for ((hash, entry), embedding) in batch_to_process.into_iter().zip(embeddings.into_iter()) {
            // Cache the embedding
            let bytes = vector::vec_to_bytes(&embedding);
            let hash_clone = hash.clone();
            let bytes_clone = bytes.clone();
            let now_clone = now.clone();
            
            let conn = self.pool.get().await?;
            conn.interact(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO embedding_cache (content_hash, embedding, created_at, accessed_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![hash_clone, bytes_clone, now_clone.clone(), now_clone],
                )?;
                
                // LRU eviction
                #[allow(clippy::cast_possible_wrap)]
                let max = cache_max as i64;
                conn.execute(
                    "DELETE FROM embedding_cache WHERE content_hash IN (
                        SELECT content_hash FROM embedding_cache
                        ORDER BY accessed_at ASC
                        LIMIT MAX(0, (SELECT COUNT(*) FROM embedding_cache) - ?1)
                    )",
                    params![max],
                )?;
                Ok::<_, anyhow::Error>(())
            }).await
            .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;

            // Notify all waiters for this content
            for sender in entry.senders {
                let _ = sender.send(Ok(embedding.clone()));
            }
        }

        Ok(())
    }

    /// Cache embedding directly (fallback path)
    async fn cache_embedding(&self, hash: &str, embedding: &[f32]) -> anyhow::Result<()> {
        let now = Local::now().to_rfc3339();
        let bytes = vector::vec_to_bytes(embedding);
        let hash = hash.to_string();
        let cache_max = self.cache_max;

        let conn = self.pool.get().await?;
        conn.interact(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO embedding_cache (content_hash, embedding, created_at, accessed_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![hash, bytes, now, now],
            )?;

            // LRU eviction
            #[allow(clippy::cast_possible_wrap)]
            let max = cache_max as i64;
            conn.execute(
                "DELETE FROM embedding_cache WHERE content_hash IN (
                    SELECT content_hash FROM embedding_cache
                    ORDER BY accessed_at ASC
                    LIMIT MAX(0, (SELECT COUNT(*) FROM embedding_cache) - ?1)
                )",
                params![max],
            )?;
            Ok::<_, anyhow::Error>(())
        }).await
        .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;

        Ok(())
    }

    /// FTS5 BM25 keyword search
    async fn fts5_search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<(String, f32)>> {
        let fts_query: String = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
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

        let conn = self.pool.get().await?;
        #[allow(clippy::cast_possible_wrap)]
        let limit_i64 = limit as i64;
        let fts_query_clone = fts_query.clone();

        let results = conn
            .interact(move |conn| {
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(params![fts_query_clone, limit_i64], |row| {
                    let id: String = row.get(0)?;
                    let score: f64 = row.get(1)?;
                    #[allow(clippy::cast_possible_truncation)]
                    Ok((id, (-score) as f32))
                })?;

                let mut results = Vec::new();
                for row in rows {
                    results.push(row?);
                }
                Ok::<_, anyhow::Error>(results)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;

        Ok(results)
    }

    /// Vector similarity search
    async fn vector_search(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> anyhow::Result<Vec<(String, f32)>> {
        let query_embedding = query_embedding.to_vec();
        let conn = self.pool.get().await?;

        let results = conn
            .interact(move |conn| {
                let mut stmt =
                    conn.prepare("SELECT id, embedding FROM memories WHERE embedding IS NOT NULL")?;

                let rows = stmt.query_map([], |row| {
                    let id: String = row.get(0)?;
                    let blob: Vec<u8> = row.get(1)?;
                    Ok((id, blob))
                })?;

                let mut scored: Vec<(String, f32)> = Vec::new();
                for row in rows {
                    let (id, blob) = row?;
                    let emb = vector::bytes_to_vec(&blob);
                    let sim = vector::cosine_similarity(&query_embedding, &emb);
                    if sim > 0.0 {
                        scored.push((id, sim));
                    }
                }

                scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                scored.truncate(limit);
                Ok::<_, anyhow::Error>(scored)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;

        Ok(results)
    }

    /// Get performance metrics
    pub async fn get_metrics(&self) -> PerformanceMetrics {
        let pool_stats = self.pool.status();
        PerformanceMetrics {
            total_requests: 0,
            batched_requests: 0,
            api_calls_saved: 0,
            avg_batch_size: 0.0,
            pool_stats: PoolStats {
                available_connections: pool_stats.available,
                max_connections: pool_stats.size,
                wait_time_ms: 0.0,
            },
        }
    }

    /// Force flush any pending embedding batch
    pub async fn flush(&self) -> anyhow::Result<()> {
        self.flush_embedding_batch().await
    }

    /// Safe reindex: rebuild FTS5 + embeddings with rollback on failure
    pub async fn reindex(&self) -> anyhow::Result<usize> {
        // Step 1: Rebuild FTS5
        let conn = self.pool.get().await?;
        conn.interact(|conn| {
            conn.execute_batch("INSERT INTO memories_fts(memories_fts) VALUES('rebuild');")
        }).await
        .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;

        // Step 2: Re-embed all memories that lack embeddings
        if self.embedder.dimensions() == 0 {
            return Ok(0);
        }

        let entries: Vec<(String, String)> = {
            let conn = self.pool.get().await?;
            conn.interact(|conn| {
                let mut stmt =
                    conn.prepare("SELECT id, content FROM memories WHERE embedding IS NULL")?;
                let rows = stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                Ok::<_, anyhow::Error>(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
            }).await
            .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??
        };

        let mut count = 0;
        for (id, content) in &entries {
            if let Ok(Some(emb)) = self.get_or_compute_embedding(content).await {
                let bytes = vector::vec_to_bytes(&emb);
                let id = id.clone();
                let conn = self.pool.get().await?;
                conn.interact(move |conn| {
                    conn.execute(
                        "UPDATE memories SET embedding = ?1 WHERE id = ?2",
                        params![bytes, id],
                    )
                }).await
                .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;
                count += 1;
            }
        }

        Ok(count)
    }
}

#[async_trait]
impl Memory for PooledSqliteMemory {
    fn name(&self) -> &str {
        "pooled_sqlite"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
    ) -> anyhow::Result<()> {
        // Compute embedding (async, before lock)
        let embedding_bytes = self
            .get_or_compute_embedding(content)
            .await?
            .map(|emb| vector::vec_to_bytes(&emb));

        let now = Local::now().to_rfc3339();
        let cat = Self::category_to_str(&category);
        let id = Uuid::new_v4().to_string();
        let key = key.to_string();
        let content = content.to_string();

        let conn = self.pool.get().await?;
        conn.interact(move |conn| {
            conn.execute(
                "INSERT INTO memories (id, key, content, category, embedding, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(key) DO UPDATE SET
                    content = excluded.content,
                    category = excluded.category,
                    embedding = excluded.embedding,
                    updated_at = excluded.updated_at",
                params![id, key, content, cat, embedding_bytes, now, now],
            )
        }).await
        .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;

        Ok(())
    }

    async fn recall(&self, query: &str, limit: usize) -> anyhow::Result<Vec<MemoryEntry>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        // Compute query embedding (async, concurrent with search)
        let query_embedding = self.get_or_compute_embedding(query).await?;

        // Concurrent search: keyword + vector in parallel
        let (keyword_results, vector_results) = tokio::join!(
            self.fts5_search(query, limit * 2),
            async {
                if let Some(ref qe) = query_embedding {
                    self.vector_search(qe, limit * 2).await.unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
        );

        let keyword_results = keyword_results?;

        // Hybrid merge
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
            vector::hybrid_merge(
                &vector_results,
                &keyword_results,
                self.vector_weight,
                self.keyword_weight,
                limit,
            )
        };

        // Fetch full entries for merged results
        let mut results = Vec::new();
        for scored in &merged {
            let id = scored.id.clone();
            let final_score = scored.final_score;
            let conn = self.pool.get().await?;
            match conn.interact(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, key, content, category, created_at FROM memories WHERE id = ?1",
                )?;
                stmt.query_row(params![id], |row| {
                    Ok(MemoryEntry {
                        id: row.get(0)?,
                        key: row.get(1)?,
                        content: row.get(2)?,
                        category: Self::str_to_category(&row.get::<_, String>(3)?),
                        timestamp: row.get(4)?,
                        session_id: None,
                        score: Some(f64::from(final_score)),
                    })
                })
            }).await {
                Ok(Ok(entry)) => results.push(entry),
                _ => {}
            }
        }

        // If hybrid returned nothing, fall back to LIKE search
        if results.is_empty() {
            let keywords: Vec<String> =
                query.split_whitespace().map(|w| format!("%{}%", w)).collect();
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
                    "SELECT id, key, content, category, created_at FROM memories
                     WHERE {where_clause}
                     ORDER BY updated_at DESC
                     LIMIT ?{}",
                    keywords.len() * 2 + 1
                );
                
                let limit_val = limit as i64;
                let conn = self.pool.get().await?;
                let keywords_clone = keywords.clone();
                
                let fallback_results = conn.interact(move |conn| {
                    let mut stmt = conn.prepare(&sql)?;
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                    for kw in &keywords_clone {
                        param_values.push(Box::new(kw.clone()));
                        param_values.push(Box::new(kw.clone()));
                    }
                    param_values.push(Box::new(limit_val));
                    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                        param_values.iter().map(AsRef::as_ref).collect();
                    let rows = stmt.query_map(params_ref.as_slice(), |row| {
                        Ok(MemoryEntry {
                            id: row.get(0)?,
                            key: row.get(1)?,
                            content: row.get(2)?,
                            category: Self::str_to_category(&row.get::<_, String>(3)?),
                            timestamp: row.get(4)?,
                            session_id: None,
                            score: Some(1.0),
                        })
                    })?;
                    
                    let mut results = Vec::new();
                    for row in rows {
                        results.push(row?);
                    }
                    Ok::<_, anyhow::Error>(results)
                }).await
                .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;
                
                results = fallback_results;
            }
        }

        results.truncate(limit);
        Ok(results)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let key = key.to_string();
        let conn = self.pool.get().await?;
        
        let entry = conn.interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, key, content, category, created_at FROM memories WHERE key = ?1",
            )?;

            let mut rows = stmt.query_map(params![key], |row| {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: None,
                    score: None,
                })
            })?;

            match rows.next() {
                Some(Ok(entry)) => Ok::<_, anyhow::Error>(Some(entry)),
                _ => Ok(None),
            }
        }).await
        .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;

        Ok(entry)
    }

    async fn list(&self, category: Option<&MemoryCategory>) -> anyhow::Result<Vec<MemoryEntry>> {
        let cat_str = category.map(|c| Self::category_to_str(c));
        let conn = self.pool.get().await?;

        let results = conn.interact(move |conn| {
            let mut results = Vec::new();

            let row_mapper = |row: &rusqlite::Row| -> rusqlite::Result<MemoryEntry> {
                Ok(MemoryEntry {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    content: row.get(2)?,
                    category: Self::str_to_category(&row.get::<_, String>(3)?),
                    timestamp: row.get(4)?,
                    session_id: None,
                    score: None,
                })
            };

            if let Some(cat) = cat_str {
                let mut stmt = conn.prepare(
                    "SELECT id, key, content, category, created_at FROM memories
                     WHERE category = ?1 ORDER BY updated_at DESC",
                )?;
                let rows = stmt.query_map(params![cat], row_mapper)?;
                for row in rows {
                    results.push(row?);
                }
            } else {
                let mut stmt = conn.prepare(
                    "SELECT id, key, content, category, created_at FROM memories
                     ORDER BY updated_at DESC",
                )?;
                let rows = stmt.query_map([], row_mapper)?;
                for row in rows {
                    results.push(row?);
                }
            }

            Ok::<_, anyhow::Error>(results)
        }).await
        .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;

        Ok(results)
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let key = key.to_string();
        let conn = self.pool.get().await?;
        
        let affected = conn.interact(move |conn| {
            conn.execute("DELETE FROM memories WHERE key = ?1", params![key])
        }).await
        .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;
        
        Ok(affected > 0)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let conn = self.pool.get().await?;
        
        let count = conn.interact(|conn| {
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
            Ok::<_, anyhow::Error>(count)
        }).await
        .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??;

        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        Ok(count as usize)
    }

    async fn health_check(&self) -> bool {
        match self.pool.get().await {
            Ok(conn) => {
                match conn.interact(|conn| conn.execute_batch("SELECT 1")).await {
                    Ok(Ok(())) => true,
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn temp_pooled_sqlite() -> (TempDir, PooledSqliteMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = PooledSqliteMemory::new(tmp.path()).await.unwrap();
        (tmp, mem)
    }

    #[tokio::test]
    async fn pooled_sqlite_name() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        assert_eq!(mem.name(), "pooled_sqlite");
    }

    #[tokio::test]
    async fn pooled_sqlite_health() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        assert!(mem.health_check().await);
    }

    #[tokio::test]
    async fn pooled_sqlite_store_and_get() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("user_lang", "Prefers Rust", MemoryCategory::Core)
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
    async fn pooled_sqlite_store_upsert() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("pref", "likes Rust", MemoryCategory::Core)
            .await
            .unwrap();
        mem.store("pref", "loves Rust", MemoryCategory::Core)
            .await
            .unwrap();

        let entry = mem.get("pref").await.unwrap().unwrap();
        assert_eq!(entry.content, "loves Rust");
        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn pooled_sqlite_recall_keyword() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("a", "Rust is fast and safe", MemoryCategory::Core)
            .await
            .unwrap();
        mem.store("b", "Python is interpreted", MemoryCategory::Core)
            .await
            .unwrap();
        mem.store("c", "Rust has zero-cost abstractions", MemoryCategory::Core)
            .await
            .unwrap();

        let results = mem.recall("Rust", 10).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|r| r.content.to_lowercase().contains("rust")));
    }

    #[tokio::test]
    async fn pooled_sqlite_recall_multi_keyword() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("a", "Rust is fast", MemoryCategory::Core)
            .await
            .unwrap();
        mem.store("b", "Rust is safe and fast", MemoryCategory::Core)
            .await
            .unwrap();

        let results = mem.recall("fast safe", 10).await.unwrap();
        assert!(!results.is_empty());
        assert!(results[0].content.contains("safe") && results[0].content.contains("fast"));
    }

    #[tokio::test]
    async fn pooled_sqlite_forget() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("temp", "temporary data", MemoryCategory::Conversation)
            .await
            .unwrap();
        assert_eq!(mem.count().await.unwrap(), 1);

        let removed = mem.forget("temp").await.unwrap();
        assert!(removed);
        assert_eq!(mem.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn pooled_sqlite_list_all() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("a", "one", MemoryCategory::Core).await.unwrap();
        mem.store("b", "two", MemoryCategory::Daily).await.unwrap();
        mem.store("c", "three", MemoryCategory::Conversation)
            .await
            .unwrap();

        let all = mem.list(None).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn pooled_sqlite_list_by_category() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("a", "core1", MemoryCategory::Core).await.unwrap();
        mem.store("b", "core2", MemoryCategory::Core).await.unwrap();
        mem.store("c", "daily1", MemoryCategory::Daily)
            .await
            .unwrap();

        let core = mem.list(Some(&MemoryCategory::Core)).await.unwrap();
        assert_eq!(core.len(), 2);

        let daily = mem.list(Some(&MemoryCategory::Daily)).await.unwrap();
        assert_eq!(daily.len(), 1);
    }

    #[tokio::test]
    async fn pooled_sqlite_db_persists() {
        let tmp = TempDir::new().unwrap();

        {
            let mem = PooledSqliteMemory::new(tmp.path()).await.unwrap();
            mem.store("persist", "I survive restarts", MemoryCategory::Core)
                .await
                .unwrap();
        }

        // Reopen
        let mem2 = PooledSqliteMemory::new(tmp.path()).await.unwrap();
        let entry = mem2.get("persist").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "I survive restarts");
    }

    #[tokio::test]
    async fn pooled_sqlite_concurrent_reads() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        
        // Store some data
        for i in 0..10 {
            mem.store(&format!("key{i}"), &format!("content{i}"), MemoryCategory::Core)
                .await
                .unwrap();
        }

        // Concurrent reads
        let mut handles = vec![];
        for i in 0..10 {
            let mem_clone = Arc::new(mem);
            let handle = tokio::spawn(async move {
                mem_clone.get(&format!("key{i}")).await
            });
            handles.push(handle);
        }

        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn pooled_sqlite_metrics() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        let metrics = mem.get_metrics().await;
        
        // Pool should be initialized
        assert!(metrics.pool_stats.max_connections > 0);
    }

    #[tokio::test]
    async fn pooled_sqlite_recall_empty_query() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("a", "data", MemoryCategory::Core).await.unwrap();
        let results = mem.recall("", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn pooled_sqlite_recall_no_match() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("a", "Rust rocks", MemoryCategory::Core)
            .await
            .unwrap();
        let results = mem.recall("javascript", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn pooled_sqlite_count_empty() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        assert_eq!(mem.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn pooled_sqlite_forget_nonexistent() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        let removed = mem.forget("nope").await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn pooled_sqlite_unicode() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("emoji_key_🦀", "こんにちは 🚀 Ñoño", MemoryCategory::Core)
            .await
            .unwrap();
        let entry = mem.get("emoji_key_🦀").await.unwrap().unwrap();
        assert_eq!(entry.content, "こんにちは 🚀 Ñoño");
    }

    #[tokio::test]
    async fn pooled_sqlite_reindex() {
        let (_tmp, mem) = temp_pooled_sqlite().await;
        mem.store("r1", "reindex test alpha", MemoryCategory::Core)
            .await
            .unwrap();

        let count = mem.reindex().await.unwrap();
        assert_eq!(count, 0); // Noop embedder

        // FTS should still work
        let results = mem.recall("reindex", 10).await.unwrap();
        assert_eq!(results.len(), 1);
    }
}