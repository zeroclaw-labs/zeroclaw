//! Brain hybrid search — symbolic filter + FTS5 + cosine similarity.
//!
//! Three-stage pipeline:
//! 1. Symbolic filter: active=1, optional subject/category
//! 2. Hybrid search: FTS5 BM25 + cosine similarity → weighted merge
//! 3. Token budget: drop lowest-scored chunks until under budget

use super::db;
use super::BrainIndex;
use crate::memory::vector::{bytes_to_vec, cosine_similarity, hybrid_merge};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Default hybrid search weights.
const VECTOR_WEIGHT: f32 = 0.7;
const KEYWORD_WEIGHT: f32 = 0.3;

/// A query result chunk with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub file_path: String,
    pub chunk_key: String,
    pub content: String,
    pub score: f64,
    pub category: String,
    pub token_count: i32,
}

/// Query options for brain search.
#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub session: Option<String>,
    pub categories: Vec<String>,
    pub top_k: usize,
    pub budget: usize,
    pub query_embedding: Option<Vec<f32>>,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            session: None,
            categories: Vec::new(),
            top_k: 10,
            budget: 8000,
            query_embedding: None,
        }
    }
}

/// Execute a hybrid search on the brain index.
pub fn query(brain: &BrainIndex, text: &str, opts: &QueryOptions) -> Result<Vec<QueryResult>> {
    let conn = db::open_db(&brain.db_path)?;

    let category_refs: Vec<&str> = opts.categories.iter().map(|s| s.as_str()).collect();

    // Stage 1+2: FTS5 keyword search
    let keyword_results = db::fts_search_filtered(
        &conn,
        text,
        opts.session.as_deref(),
        &category_refs,
        opts.top_k * 3, // over-fetch for merge
    )?;

    // Stage 2: Vector search (if embedding provided)
    let vector_results = if let Some(ref query_emb) = opts.query_embedding {
        let chunks =
            db::get_chunks_with_embeddings(&conn, opts.session.as_deref(), &category_refs)?;

        let mut scored: Vec<(String, f32)> = chunks
            .iter()
            .filter_map(|chunk| {
                chunk.embedding.as_ref().map(|emb_bytes| {
                    let emb = bytes_to_vec(emb_bytes);
                    let sim = cosine_similarity(query_emb, &emb);
                    (chunk.id.to_string(), sim)
                })
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(opts.top_k * 3);
        scored
    } else {
        Vec::new()
    };

    // Convert keyword results to (id_string, score) format
    let kw_for_merge: Vec<(String, f32)> = keyword_results
        .iter()
        .map(|(id, score)| (id.to_string(), *score))
        .collect();

    // Hybrid merge
    let merged = hybrid_merge(
        &vector_results,
        &kw_for_merge,
        VECTOR_WEIGHT,
        KEYWORD_WEIGHT,
        opts.top_k * 2,
    );

    // Stage 3: Build results, apply token budget
    let mut results = Vec::new();
    let mut total_tokens = 0_usize;

    for scored in &merged {
        if results.len() >= opts.top_k {
            break;
        }

        let id: i64 = scored.id.parse().unwrap_or(0);
        if let Ok(Some(chunk)) = db::get_chunk(&conn, id) {
            let chunk_tokens = chunk.token_count.unsigned_abs() as usize;

            if total_tokens + chunk_tokens > opts.budget {
                continue;
            }

            total_tokens += chunk_tokens;
            results.push(QueryResult {
                file_path: chunk.file_path,
                chunk_key: chunk.chunk_key,
                content: chunk.content,
                score: f64::from(scored.final_score),
                category: chunk.category,
                token_count: chunk.token_count,
            });
        }
    }

    Ok(results)
}

/// Format query results as markdown for injection into prompts.
pub fn format_results_markdown(results: &[QueryResult]) -> String {
    if results.is_empty() {
        return String::new();
    }

    let total_tokens: i32 = results.iter().map(|r| r.token_count).sum();
    let mut out = format!(
        "# Brain Knowledge ({} chunks, ~{} tokens)\n\n",
        results.len(),
        total_tokens
    );

    for r in results {
        use std::fmt::Write;
        write!(
            out,
            "## {} ({})\n\n{}\n\n",
            r.chunk_key, r.file_path, r.content
        )
        .unwrap();
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_brain() -> (TempDir, BrainIndex) {
        let dir = TempDir::new().unwrap();
        let brain = BrainIndex::new(dir.path());

        let conn = db::open_db(&brain.db_path).unwrap();
        db::upsert_chunk(
            &conn,
            "knowledge/arch.md",
            "h1",
            0,
            "## Database",
            "Multi-tenant PostgreSQL with schema isolation",
            "ch1",
            7,
            "knowledge",
            "markdown",
            None,
            None,
        )
        .unwrap();
        db::upsert_chunk(
            &conn,
            "principles/quality.md",
            "h2",
            0,
            "## Testing",
            "Test what the spec says not what the implementation does",
            "ch2",
            9,
            "principle",
            "markdown",
            None,
            None,
        )
        .unwrap();
        db::upsert_chunk(
            &conn,
            "cortex/engineering/backend.yaml",
            "h3",
            0,
            "session",
            "Django backend with Ninja API and Celery",
            "ch3",
            8,
            "cortex",
            "yaml",
            Some("backend"),
            None,
        )
        .unwrap();

        (dir, brain)
    }

    #[test]
    fn query_keyword_only() {
        let (_dir, brain) = setup_test_brain();
        let opts = QueryOptions::default();
        let results = query(&brain, "PostgreSQL multi-tenant", &opts).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].file_path, "knowledge/arch.md");
    }

    #[test]
    fn query_with_category_filter() {
        let (_dir, brain) = setup_test_brain();
        let opts = QueryOptions {
            categories: vec!["principle".to_string()],
            ..QueryOptions::default()
        };
        let results = query(&brain, "testing spec", &opts).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].category, "principle");
    }

    #[test]
    fn query_with_session_filter() {
        let (_dir, brain) = setup_test_brain();
        let opts = QueryOptions {
            session: Some("backend".to_string()),
            ..QueryOptions::default()
        };
        let results = query(&brain, "Django", &opts).unwrap();
        // Should return the backend cortex chunk (subject=backend) + universal chunks
        assert!(!results.is_empty());
    }

    #[test]
    fn query_respects_budget() {
        let (_dir, brain) = setup_test_brain();
        let opts = QueryOptions {
            budget: 5, // Very tight budget — only 5 tokens
            ..QueryOptions::default()
        };
        let results = query(&brain, "Django PostgreSQL testing", &opts).unwrap();
        let total: i32 = results.iter().map(|r| r.token_count).sum();
        assert!(total <= 5);
    }

    #[test]
    fn format_results_empty() {
        assert_eq!(format_results_markdown(&[]), "");
    }

    #[test]
    fn format_results_markdown_structure() {
        let results = vec![QueryResult {
            file_path: "knowledge/arch.md".to_string(),
            chunk_key: "## Database".to_string(),
            content: "Multi-tenant PostgreSQL".to_string(),
            score: 0.85,
            category: "knowledge".to_string(),
            token_count: 4,
        }];
        let md = format_results_markdown(&results);
        assert!(md.contains("# Brain Knowledge"));
        assert!(md.contains("## Database"));
        assert!(md.contains("knowledge/arch.md"));
    }
}
