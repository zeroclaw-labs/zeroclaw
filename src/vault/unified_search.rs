// @Ref: SUMMARY §5 — parallel first + second brain search + RRF merge.
//
// Runs `Memory::recall` (first brain episodes) and `VaultStore::search_fts`
// (second brain documents) concurrently via tokio::join!, merges the two
// ranked lists with Reciprocal Rank Fusion, and logs the retrieval event
// to `chat_retrieval_logs` so the "both-brains-always-queried" invariant
// is auditable.

use super::store::VaultStore;
use crate::memory::Memory;
use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchScope {
    Both,
    FirstOnly,
    SecondOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitSource {
    FirstBrain,
    SecondBrain,
}

#[derive(Debug, Clone)]
pub struct UnifiedHit {
    pub source: HitSource,
    /// Opaque id: memory key for first brain, vault_doc_id for second.
    pub ref_id: String,
    pub title: String,
    pub snippet: String,
    pub score: f32,
    /// Debug breadcrumb for investigating ranker behaviour.
    pub ranker_trace: String,
}

/// Run a unified query against both brains.
///
/// - `chat_msg_id`: optional upstream chat identifier for retrieval log.
/// - `scope`: Both (default) runs both searches in parallel; FirstOnly /
///   SecondOnly runs only the requested side. Always logs.
/// - `top_k`: final merged result count (per-side fetches 2*top_k before RRF).
///
/// Both-side execution uses `tokio::join!` and a 300 ms per-side soft timeout.
/// If one side errors, the other's results are returned alone (degraded mode)
/// and the error is logged at warn level.
pub async fn unified_search(
    memory: &dyn Memory,
    vault: &VaultStore,
    query: &str,
    session_id: Option<&str>,
    scope: SearchScope,
    top_k: usize,
    chat_msg_id: Option<&str>,
) -> Result<Vec<UnifiedHit>> {
    let started = Instant::now();
    let per_side = (top_k * 2).max(10);

    let first_fut = async {
        if matches!(scope, SearchScope::SecondOnly) {
            Ok::<Vec<UnifiedHit>, anyhow::Error>(Vec::new())
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_millis(300),
                memory.recall(query, per_side, session_id),
            )
            .await
            {
                Ok(Ok(hits)) => Ok(hits
                    .into_iter()
                    .enumerate()
                    .map(|(rank, e)| UnifiedHit {
                        source: HitSource::FirstBrain,
                        ref_id: e.key.clone(),
                        title: e.key.clone(),
                        snippet: truncate(&e.content, 200),
                        score: e.score.map(|s| s as f32).unwrap_or(0.0),
                        ranker_trace: format!("first_brain:rank={}", rank + 1),
                    })
                    .collect()),
                Ok(Err(e)) => {
                    tracing::warn!("first_brain recall error: {e}");
                    Ok(Vec::new())
                }
                Err(_) => {
                    tracing::warn!("first_brain recall timed out");
                    Ok(Vec::new())
                }
            }
        }
    };

    let second_fut = async {
        if matches!(scope, SearchScope::FirstOnly) {
            Ok::<Vec<UnifiedHit>, anyhow::Error>(Vec::new())
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_millis(300),
                async { vault.search_fts(query, per_side) },
            )
            .await
            {
                Ok(Ok(rows)) => Ok(rows
                    .into_iter()
                    .enumerate()
                    .map(|(rank, (doc_id, title, score))| UnifiedHit {
                        source: HitSource::SecondBrain,
                        ref_id: doc_id.to_string(),
                        title,
                        snippet: String::new(),
                        score,
                        ranker_trace: format!("second_brain:rank={}", rank + 1),
                    })
                    .collect()),
                Ok(Err(e)) => {
                    tracing::warn!("second_brain search error: {e}");
                    Ok(Vec::new())
                }
                Err(_) => {
                    tracing::warn!("second_brain search timed out");
                    Ok(Vec::new())
                }
            }
        }
    };

    let (first_result, second_result) = tokio::join!(first_fut, second_fut);
    let first_hits: Vec<UnifiedHit> = first_result?;
    let second_hits: Vec<UnifiedHit> = second_result?;

    let merged = rrf_merge(&first_hits, &second_hits, 60.0, top_k);
    let latency_ms = started.elapsed().as_millis() as i64;

    // Always log the retrieval event (audit invariant §9-5 of plan).
    let _ = log_retrieval(
        vault,
        chat_msg_id,
        query,
        first_hits.len() as i64,
        second_hits.len() as i64,
        latency_ms,
        &merged,
    );

    Ok(merged)
}

/// RRF merge: score(doc) = Σ 1/(k + rank_i). Top docs from either side
/// accumulate across both rankings.
fn rrf_merge(
    first: &[UnifiedHit],
    second: &[UnifiedHit],
    k: f32,
    limit: usize,
) -> Vec<UnifiedHit> {
    let mut score_by_key: HashMap<String, (f32, UnifiedHit)> = HashMap::new();

    for (rank0, hit) in first.iter().enumerate() {
        let key = dedup_key(hit);
        let contribution = 1.0 / (k + (rank0 + 1) as f32);
        score_by_key
            .entry(key)
            .and_modify(|(s, _)| *s += contribution)
            .or_insert((contribution, hit.clone()));
    }
    for (rank0, hit) in second.iter().enumerate() {
        let key = dedup_key(hit);
        let contribution = 1.0 / (k + (rank0 + 1) as f32);
        score_by_key
            .entry(key)
            .and_modify(|(s, _)| *s += contribution)
            .or_insert((contribution, hit.clone()));
    }

    let mut merged: Vec<UnifiedHit> = score_by_key
        .into_values()
        .map(|(score, mut hit)| {
            hit.score = score;
            hit
        })
        .collect();
    merged.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    merged.truncate(limit);
    merged
}

fn dedup_key(hit: &UnifiedHit) -> String {
    match hit.source {
        HitSource::FirstBrain => format!("first:{}", hit.ref_id),
        HitSource::SecondBrain => format!("second:{}", hit.ref_id),
    }
}

fn truncate(s: &str, n: usize) -> String {
    let mut out = String::with_capacity(n);
    for (i, c) in s.chars().enumerate() {
        if i >= n {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

fn log_retrieval(
    vault: &VaultStore,
    chat_msg_id: Option<&str>,
    query: &str,
    first_hits: i64,
    second_hits: i64,
    latency_ms: i64,
    merged: &[UnifiedHit],
) -> Result<()> {
    let refs_json = serde_json::to_string(
        &merged
            .iter()
            .take(10)
            .map(|h| {
                serde_json::json!({
                    "source": match h.source {
                        HitSource::FirstBrain => "first",
                        HitSource::SecondBrain => "second",
                    },
                    "ref_id": h.ref_id,
                    "score": h.score,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string());

    let conn = vault.connection().lock();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    conn.execute(
        "INSERT INTO chat_retrieval_logs
            (chat_msg_id, query, first_brain_hits, second_brain_hits, latency_ms,
             merged_top_refs, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            chat_msg_id,
            query,
            first_hits,
            second_hits,
            latency_ms,
            refs_json,
            now as i64,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::sqlite::SqliteMemory;
    use crate::memory::traits::MemoryCategory;
    use crate::vault::ingest::{IngestInput, SourceType};
    use crate::vault::store::VaultStore;

    async fn setup() -> (tempfile::TempDir, std::sync::Arc<SqliteMemory>, VaultStore) {
        let tmp = tempfile::TempDir::new().unwrap();
        let mem = std::sync::Arc::new(SqliteMemory::new(tmp.path()).unwrap());
        // Vault uses its own separate in-memory conn for the test — real
        // production shares brain.db, but test isolation is clearer this way.
        let vault_conn = std::sync::Arc::new(parking_lot::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        ));
        let vault = VaultStore::with_shared_connection(vault_conn).unwrap();
        (tmp, mem, vault)
    }

    #[tokio::test]
    async fn unified_search_queries_both_brains_in_parallel() {
        let (_tmp, mem, vault) = setup().await;

        // First brain: store an episode mentioning "손해배상".
        mem.store("ep1", "오늘 손해배상 상담을 했다", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Second brain: ingest a legal doc about 민법 제750조.
        vault
            .ingest_markdown(IngestInput {
                source_type: SourceType::LocalFile,
                source_device_id: "dev",
                original_path: None,
                title: Some("민법 제750조 해설"),
                markdown: &long_legal_doc(),
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap();

        let hits = unified_search(
            mem.as_ref(),
            &vault,
            "손해배상 민법",
            None,
            SearchScope::Both,
            10,
            Some("msg-1"),
        )
        .await
        .unwrap();

        // Both sides contributed at least once.
        assert!(hits.iter().any(|h| matches!(h.source, HitSource::FirstBrain)));
        assert!(hits.iter().any(|h| matches!(h.source, HitSource::SecondBrain)));

        // Log was written.
        let log_count: i64 = {
            let c = vault.connection().lock();
            c.query_row("SELECT COUNT(*) FROM chat_retrieval_logs", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(log_count, 1);
    }

    #[tokio::test]
    async fn scope_first_only_skips_second() {
        let (_tmp, mem, vault) = setup().await;
        mem.store("ep1", "hello world", MemoryCategory::Core, None)
            .await
            .unwrap();
        let hits = unified_search(
            mem.as_ref(),
            &vault,
            "hello",
            None,
            SearchScope::FirstOnly,
            10,
            None,
        )
        .await
        .unwrap();
        assert!(hits.iter().all(|h| matches!(h.source, HitSource::FirstBrain)));

        // Log still written, second count = 0.
        let second_hits: i64 = {
            let c = vault.connection().lock();
            c.query_row(
                "SELECT second_brain_hits FROM chat_retrieval_logs",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(second_hits, 0);
    }

    fn long_legal_doc() -> String {
        let body = "민법 제750조는 불법행위에 관한 핵심 조항이다. 손해배상 청구의 근거가 된다. ".repeat(50);
        format!("# 민법 제750조 해설\n\n{body}")
    }
}
