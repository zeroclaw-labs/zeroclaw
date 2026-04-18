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
    /// Second brain via vector similarity dimension.
    SecondBrainVector,
    /// Second brain via wikilink graph BFS dimension.
    SecondBrainGraph,
    /// Second brain via frontmatter meta filter dimension.
    SecondBrainMeta,
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

/// Classify the query into one of four archetypes so the weight router
/// can emphasise the most relevant dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    /// "2024가합12345" — exact case number lookup; meta heaviest.
    CaseNumber,
    /// "민법 제750조 적용 사례" — statute citation; hub/meta heavy (vector secondary).
    StatuteArticle,
    /// "피고 홍길동 관련" — person name; graph heavy.
    Person,
    /// "투자사기 판례 경향" — conceptual; vector heavy.
    Concept,
}

impl QueryKind {
    /// Heuristic classification matching the Plan §8 adaptive-weight table.
    pub fn classify(query: &str) -> Self {
        use crate::vault::wikilink::tokens;
        let compounds = tokens::detect_compound_tokens(query);
        use crate::vault::wikilink::tokens::CompoundTokenKind;
        if let Some(c) = compounds.first() {
            match c.kind {
                CompoundTokenKind::CaseNumber => return Self::CaseNumber,
                CompoundTokenKind::StatuteArticle => return Self::StatuteArticle,
                CompoundTokenKind::PrecedentCitation => return Self::StatuteArticle,
                CompoundTokenKind::Organization => return Self::Person,
            }
        }
        // Very lightweight Korean name heuristic: surname+1~2 chars.
        static KOREAN_SURNAMES: &[&str] = &[
            "김", "이", "박", "최", "정", "강", "조", "윤", "장", "임",
            "한", "오", "서", "신", "권", "황", "안", "송", "류", "전",
            "홍", "고", "문", "양", "손", "배", "백", "허", "남", "심",
        ];
        let trimmed = query.trim();
        if let Some(first) = trimmed.chars().next() {
            if KOREAN_SURNAMES.contains(&first.to_string().as_str())
                && trimmed.chars().count() >= 2
                && trimmed.chars().count() <= 4
            {
                return Self::Person;
            }
        }
        Self::Concept
    }

    /// Weights per RAG dimension for this query type. Plan §8 Part IV.
    /// Returns (hub, vector, graph, meta). Hub slot is reserved for
    /// Phase 2 `hub_notes`; currently contributes 0 unless a hub
    /// collapses to a matching vault_documents row.
    pub fn weights(&self) -> (f32, f32, f32, f32) {
        match self {
            Self::StatuteArticle => (0.5, 0.2, 0.1, 0.2),
            Self::CaseNumber => (0.1, 0.1, 0.2, 0.6),
            Self::Concept => (0.3, 0.4, 0.2, 0.1),
            Self::Person => (0.2, 0.1, 0.5, 0.2),
        }
    }
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

    let kind = QueryKind::classify(query);
    let (_w_hub, w_vector, w_graph, w_meta) = kind.weights();

    // ── First brain (single dimension: episodes via Memory::recall) ───
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

    // ── Second brain: 3 parallel dimensions (FTS, vector, graph) + meta ──
    let run_second = !matches!(scope, SearchScope::FirstOnly);

    let fts_fut = async {
        if !run_second {
            return Vec::<UnifiedHit>::new();
        }
        match tokio::time::timeout(
            std::time::Duration::from_millis(300),
            async { vault.search_fts(query, per_side) },
        )
        .await
        {
            Ok(Ok(rows)) => rows
                .into_iter()
                .enumerate()
                .map(|(rank, (doc_id, title, score))| UnifiedHit {
                    source: HitSource::SecondBrain,
                    ref_id: doc_id.to_string(),
                    title,
                    snippet: String::new(),
                    score,
                    ranker_trace: format!("second_fts:rank={}", rank + 1),
                })
                .collect(),
            _ => Vec::new(),
        }
    };

    let vector_fut = async {
        if !run_second {
            return Vec::<UnifiedHit>::new();
        }
        match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            vault.search_vector(query, per_side),
        )
        .await
        {
            Ok(Ok(rows)) => rows
                .into_iter()
                .enumerate()
                .map(|(rank, (doc_id, title, score))| UnifiedHit {
                    source: HitSource::SecondBrainVector,
                    ref_id: doc_id.to_string(),
                    title,
                    snippet: String::new(),
                    score,
                    ranker_trace: format!("second_vec:rank={}", rank + 1),
                })
                .collect(),
            _ => Vec::new(),
        }
    };

    let graph_fut = async {
        if !run_second {
            return Vec::<UnifiedHit>::new();
        }
        match tokio::time::timeout(std::time::Duration::from_millis(300), async {
            vault.search_graph(query, 2, per_side)
        })
        .await
        {
            Ok(Ok(rows)) => rows
                .into_iter()
                .enumerate()
                .map(|(rank, (doc_id, title, score))| UnifiedHit {
                    source: HitSource::SecondBrainGraph,
                    ref_id: doc_id.to_string(),
                    title,
                    snippet: String::new(),
                    score,
                    ranker_trace: format!("second_graph:rank={}", rank + 1),
                })
                .collect(),
            _ => Vec::new(),
        }
    };

    // Meta filter: extract (case_number, <query>) if query is a case number.
    let meta_fut = async {
        if !run_second {
            return Vec::<UnifiedHit>::new();
        }
        let filters: Vec<(String, String)> = match kind {
            QueryKind::CaseNumber => vec![("case_number".into(), query.to_string())],
            // For statute queries also let meta filter on key_statutes if present.
            QueryKind::StatuteArticle => vec![("key_statutes".into(), query.to_string())],
            _ => Vec::new(),
        };
        if filters.is_empty() {
            return Vec::new();
        }
        match tokio::time::timeout(std::time::Duration::from_millis(200), async {
            vault.search_meta(&filters, per_side)
        })
        .await
        {
            Ok(Ok(rows)) => rows
                .into_iter()
                .enumerate()
                .map(|(rank, (doc_id, title, score))| UnifiedHit {
                    source: HitSource::SecondBrainMeta,
                    ref_id: doc_id.to_string(),
                    title,
                    snippet: String::new(),
                    score,
                    ranker_trace: format!("second_meta:rank={}", rank + 1),
                })
                .collect(),
            _ => Vec::new(),
        }
    };

    let (first_result, fts_hits, vector_hits, graph_hits, meta_hits) =
        tokio::join!(first_fut, fts_fut, vector_fut, graph_fut, meta_fut);
    let first_hits: Vec<UnifiedHit> = first_result?;

    // Weighted RRF merge across 4 second-brain dimensions + first brain.
    // FTS shares the "graph"-like weight if no hub; for now treat FTS as
    // concept vector secondary at weight=w_vector*0.5.
    let second_weighted: Vec<(f32, Vec<UnifiedHit>)> = vec![
        (w_vector * 0.5, fts_hits),
        (w_vector, vector_hits),
        (w_graph, graph_hits),
        (w_meta, meta_hits),
    ];

    let merged = weighted_rrf_merge(&first_hits, &second_weighted, 60.0, top_k);
    let latency_ms = started.elapsed().as_millis() as i64;

    // Sum of distinct doc_ids across all 4 second-brain dimensions for the
    // audit log (dedup so repeated hits on the same doc don't inflate).
    let mut second_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (_w, hits) in &second_weighted {
        for h in hits {
            second_ids.insert(h.ref_id.clone());
        }
    }

    // Always log the retrieval event (audit invariant §9-5 of plan).
    let _ = log_retrieval(
        vault,
        chat_msg_id,
        query,
        first_hits.len() as i64,
        second_ids.len() as i64,
        latency_ms,
        &merged,
    );

    Ok(merged)
}

/// Weighted RRF: first brain contributes at its own weight (1.0 baseline);
/// each second-brain dimension contributes at `weight * 1/(k + rank)`.
/// Top docs accumulate across all rankings, then truncated to `limit`.
fn weighted_rrf_merge(
    first: &[UnifiedHit],
    second_weighted: &[(f32, Vec<UnifiedHit>)],
    k: f32,
    limit: usize,
) -> Vec<UnifiedHit> {
    let mut score_by_key: HashMap<String, (f32, UnifiedHit)> = HashMap::new();

    // First brain: weight 1.0.
    for (rank0, hit) in first.iter().enumerate() {
        let key = dedup_key(hit);
        let contribution = 1.0 / (k + (rank0 + 1) as f32);
        score_by_key
            .entry(key)
            .and_modify(|(s, _)| *s += contribution)
            .or_insert((contribution, hit.clone()));
    }

    // Second brain dimensions: each at its configured weight.
    for (weight, hits) in second_weighted {
        for (rank0, hit) in hits.iter().enumerate() {
            let key = dedup_key(hit);
            let contribution = weight * (1.0 / (k + (rank0 + 1) as f32));
            score_by_key
                .entry(key)
                .and_modify(|(s, _)| *s += contribution)
                .or_insert((contribution, hit.clone()));
        }
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
        // All second-brain dimensions dedup against the same doc_id so
        // a doc hit by both FTS and vector accumulates its score rather
        // than appearing twice.
        HitSource::SecondBrain
        | HitSource::SecondBrainVector
        | HitSource::SecondBrainGraph
        | HitSource::SecondBrainMeta => format!("second:{}", hit.ref_id),
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
                        HitSource::SecondBrain => "second_fts",
                        HitSource::SecondBrainVector => "second_vec",
                        HitSource::SecondBrainGraph => "second_graph",
                        HitSource::SecondBrainMeta => "second_meta",
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

    #[test]
    fn classify_case_number() {
        assert_eq!(QueryKind::classify("2024가합12345"), QueryKind::CaseNumber);
    }

    #[test]
    fn classify_statute_article() {
        assert_eq!(
            QueryKind::classify("민법 제750조 적용"),
            QueryKind::StatuteArticle
        );
    }

    #[test]
    fn classify_person_by_korean_surname() {
        assert_eq!(QueryKind::classify("홍길동"), QueryKind::Person);
    }

    #[test]
    fn classify_concept_fallback() {
        assert_eq!(QueryKind::classify("투자사기 판례 경향"), QueryKind::Concept);
    }

    #[test]
    fn weights_sum_reasonable() {
        for k in [
            QueryKind::CaseNumber,
            QueryKind::StatuteArticle,
            QueryKind::Person,
            QueryKind::Concept,
        ] {
            let (h, v, g, m) = k.weights();
            let sum = h + v + g + m;
            assert!((sum - 1.0).abs() < 1e-5, "weights must sum to 1.0 for {k:?}");
        }
    }

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
        assert!(hits.iter().any(|h| matches!(
            h.source,
            HitSource::SecondBrain
                | HitSource::SecondBrainVector
                | HitSource::SecondBrainGraph
                | HitSource::SecondBrainMeta
        )));

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
