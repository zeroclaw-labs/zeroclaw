//! RAG ingest pipeline.
//!
//! Orchestrates content ingestion:
//! 1. Compute a content hash to detect unchanged sources.
//! 2. Look up any existing source record by URL.
//! 3. Skip ingestion if content has not changed.
//! 4. Chunk the content.
//! 5. Embed all chunks in batches via [`NvidiaEmbeddingClient`].
//! 6. Persist source and chunks to PostgreSQL inside a logical transaction
//!    (old chunks are deleted before new ones are inserted).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use anyhow::{bail, Result};

use super::embedding::NvidiaEmbeddingClient;
use super::pgvector::{ChunkRow, InsertSourceRequest, PgVectorStore};
use super::text_chunker::{chunk_text, TextChunk};

// ── Public types ───────────────────────────────────────────────────────────────

/// Parameters for a single ingest request.
pub struct IngestRequest<'a> {
    /// Raw text content to ingest.
    pub content: &'a str,
    /// Source type label (e.g. `"web"`, `"pdf"`, `"manual"`).
    pub source_type: &'a str,
    /// Original URL of the content, if applicable.
    pub source_url: Option<&'a str>,
    /// Human-readable title.
    pub title: Option<&'a str>,
    /// Target chunk size in tokens (defaults to 500 if zero).
    pub chunk_size_tokens: usize,
    /// Overlap between consecutive chunks in tokens (defaults to 50 if zero).
    pub chunk_overlap_tokens: usize,
}

/// Outcome of an ingest call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Content was new; source and chunks were stored.
    Inserted {
        source_id: String,
        chunk_count: usize,
    },
    /// Content changed; old chunks were replaced.
    Updated {
        source_id: String,
        chunk_count: usize,
    },
    /// Content is identical to what is already stored; nothing was written.
    Unchanged { source_id: String },
    /// No source URL provided and content is not trackable by hash; ingested fresh.
    InsertedAnonymous {
        source_id: String,
        chunk_count: usize,
    },
}

// ── Pipeline entry point ───────────────────────────────────────────────────────

/// Ingest a document into the RAG store.
///
/// Returns an [`IngestOutcome`] describing what happened.
pub async fn ingest(
    store: &PgVectorStore,
    embedder: &NvidiaEmbeddingClient,
    req: &IngestRequest<'_>,
) -> Result<IngestOutcome> {
    if req.content.trim().is_empty() {
        bail!("ingest content must not be empty");
    }

    let chunk_size = if req.chunk_size_tokens == 0 {
        500
    } else {
        req.chunk_size_tokens
    };
    let overlap = if req.chunk_overlap_tokens == 0 {
        50
    } else {
        req.chunk_overlap_tokens
    };

    let content_hash = compute_hash(req.content);

    // ── Check for existing source ──────────────────────────────────────────────
    let existing = if let Some(url) = req.source_url {
        store.find_source_by_url(url).await?
    } else {
        None
    };

    if let Some(ref src) = existing {
        // If the hash matches, skip re-ingestion.
        if src
            .content_hash
            .as_deref()
            .map_or(false, |h| h == content_hash)
        {
            return Ok(IngestOutcome::Unchanged {
                source_id: src.id.clone(),
            });
        }
    }

    // ── Chunk and embed ────────────────────────────────────────────────────────
    let chunks = chunk_text(req.content, chunk_size, overlap);
    if chunks.is_empty() {
        bail!("document produced no chunks after splitting");
    }

    let chunk_texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let embeddings = embedder.embed_passages(&chunk_texts).await?;

    if embeddings.len() != chunks.len() {
        bail!(
            "embedding count ({}) does not match chunk count ({})",
            embeddings.len(),
            chunks.len()
        );
    }

    let chunk_rows = build_chunk_rows(&chunks, embeddings);

    // ── Persist ────────────────────────────────────────────────────────────────
    if let Some(src) = existing {
        // Update existing source: delete old chunks, insert new ones, update hash.
        store.delete_chunks_for_source(&src.id).await?;
        store.insert_chunks(&src.id, &chunk_rows).await?;
        store.update_source_hash(&src.id, &content_hash).await?;

        let chunk_count = chunk_rows.len();
        Ok(IngestOutcome::Updated {
            source_id: src.id,
            chunk_count,
        })
    } else {
        // Insert new source.
        let insert_req = InsertSourceRequest {
            source_type: req.source_type.to_string(),
            source_url: req.source_url.map(str::to_string),
            title: req.title.map(str::to_string),
            content_hash: Some(content_hash),
        };
        let source_id = store.insert_source(&insert_req).await?;
        store.insert_chunks(&source_id, &chunk_rows).await?;

        let chunk_count = chunk_rows.len();
        if req.source_url.is_some() {
            Ok(IngestOutcome::Inserted {
                source_id,
                chunk_count,
            })
        } else {
            Ok(IngestOutcome::InsertedAnonymous {
                source_id,
                chunk_count,
            })
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Build [`ChunkRow`]s by zipping chunks and their embeddings.
fn build_chunk_rows(chunks: &[TextChunk], embeddings: Vec<Vec<f32>>) -> Vec<ChunkRow> {
    chunks
        .iter()
        .zip(embeddings)
        .map(|(chunk, embedding)| ChunkRow {
            chunk_index: chunk.chunk_index,
            content: chunk.content.clone(),
            heading_context: chunk.heading_context.clone(),
            embedding,
            token_count: Some(chunk.approximate_tokens),
        })
        .collect()
}

/// Compute a simple deterministic hash of `content` for change detection.
///
/// Uses Rust's [`DefaultHasher`] (fast, not cryptographic).  For the purpose
/// of detecting unchanged documents this is sufficient.
fn compute_hash(content: &str) -> String {
    let mut h = DefaultHasher::new();
    content.hash(&mut h);
    format!("{:x}", h.finish())
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_hash_is_deterministic() {
        let a = compute_hash("hello world");
        let b = compute_hash("hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn compute_hash_differs_for_different_content() {
        let a = compute_hash("content A");
        let b = compute_hash("content B");
        assert_ne!(a, b);
    }

    #[test]
    fn compute_hash_is_hex_string() {
        let h = compute_hash("test");
        assert!(
            h.chars().all(|c| c.is_ascii_hexdigit()),
            "hash should be hex: {h}"
        );
    }

    #[test]
    fn build_chunk_rows_zips_correctly() {
        let chunks = vec![
            TextChunk {
                content: "first chunk".into(),
                heading_context: Some("## Section".into()),
                chunk_index: 0,
                approximate_tokens: 3,
            },
            TextChunk {
                content: "second chunk".into(),
                heading_context: None,
                chunk_index: 1,
                approximate_tokens: 3,
            },
        ];
        let embeddings = vec![vec![0.1f32; 4], vec![0.2f32; 4]];
        let rows = build_chunk_rows(&chunks, embeddings);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].chunk_index, 0);
        assert_eq!(rows[0].content, "first chunk");
        assert_eq!(rows[0].heading_context.as_deref(), Some("## Section"));
        assert_eq!(rows[0].token_count, Some(3));

        assert_eq!(rows[1].chunk_index, 1);
        assert!(rows[1].heading_context.is_none());
    }
}
