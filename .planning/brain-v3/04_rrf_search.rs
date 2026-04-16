// ============================================================
// MoA v3.0 — S1: RRF Hybrid Search Skeleton
// Location: src/memory/hybrid.rs (신규)
//
// Claude Code 지침:
//  - 이 파일은 스켈레톤이다. TODO 주석 위치만 채우면 된다.
//  - 기존 가중합 검색 함수는 삭제하지 말고, 본 모듈이 config.search_mode
//    에 따라 분기하여 호출하도록 연결한다.
//  - 테스트: tests/memory_hybrid_rrf.rs 에 단위테스트 추가.
// ============================================================

use std::collections::HashMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::memory::{
    embedding::embed_text,
    repo::{keyword_search_fts, vector_search, MemoryRow},
};

// ------------------------------------------------------------
// Config
// ------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    /// 기존 0.7*vec + 0.3*fts 가중합
    Weighted,
    /// Reciprocal Rank Fusion (v3.0 신규, 기본 k=60)
    Rrf,
}

impl Default for SearchMode {
    fn default() -> Self {
        // 벤치마크 완료 전까지는 Weighted 가 기본
        SearchMode::Weighted
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HybridSearchConfig {
    pub mode: SearchMode,
    pub top_k: usize,
    pub rrf_k: f32,                  // RRF 상수, 기본 60.0
    pub vector_weight: f32,          // Weighted 모드 전용
    pub keyword_weight: f32,         // Weighted 모드 전용
    pub per_ranker_candidates: usize,// 각 랭커가 뽑아올 후보 수 (기본 100)
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            mode: SearchMode::default(),
            top_k: 20,
            rrf_k: 60.0,
            vector_weight: 0.7,
            keyword_weight: 0.3,
            per_ranker_candidates: 100,
        }
    }
}

// ------------------------------------------------------------
// 결과 타입
// ------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ScoredMemory {
    pub memory: MemoryRow,
    pub score: f32,
    /// 진단용: 각 랭커에서의 순위 (1-based). 없으면 미발견.
    pub ranks: HashMap<&'static str, usize>,
}

// ------------------------------------------------------------
// 공개 API
// ------------------------------------------------------------

/// Phase 1 하이브리드 검색 진입점.
/// 기존 `src/memory/search.rs` 의 `phase1_memory_recall` 이 본 함수로 교체된다.
pub async fn hybrid_search(
    db: &crate::memory::Db,
    query: &str,
    queries_expanded: Option<&[String]>,  // S3 multi-query 결과, None 이면 단일
    config: &HybridSearchConfig,
) -> Result<Vec<ScoredMemory>> {
    match config.mode {
        SearchMode::Weighted => weighted_search(db, query, queries_expanded, config).await,
        SearchMode::Rrf => rrf_search(db, query, queries_expanded, config).await,
    }
}

// ------------------------------------------------------------
// RRF 구현
// ------------------------------------------------------------

async fn rrf_search(
    db: &crate::memory::Db,
    query: &str,
    queries_expanded: Option<&[String]>,
    config: &HybridSearchConfig,
) -> Result<Vec<ScoredMemory>> {
    let queries: Vec<String> = match queries_expanded {
        Some(qs) if !qs.is_empty() => qs.to_vec(),
        _ => vec![query.to_string()],
    };

    // 각 쿼리 × {vec, fts} 조합 → ranked list 수집
    let mut ranked_lists: Vec<(&'static str, Vec<MemoryRow>)> = Vec::new();

    for q in &queries {
        // 벡터 검색
        let embedding = embed_text(q).await?;
        let vec_hits = vector_search(db, &embedding, config.per_ranker_candidates).await?;
        ranked_lists.push(("vec", vec_hits));

        // 키워드(FTS5) 검색
        let fts_hits = keyword_search_fts(db, q, config.per_ranker_candidates).await?;
        ranked_lists.push(("fts", fts_hits));
    }

    // RRF 누적
    let mut acc: HashMap<i64, ScoredMemory> = HashMap::new();
    for (ranker_name, hits) in ranked_lists {
        for (rank_0based, mem) in hits.into_iter().enumerate() {
            let rank = rank_0based + 1; // 1-based
            let contribution = 1.0f32 / (config.rrf_k + rank as f32);

            acc.entry(mem.id)
                .and_modify(|sm| {
                    sm.score += contribution;
                    sm.ranks.insert(ranker_name, rank);
                })
                .or_insert_with(|| {
                    let mut ranks = HashMap::new();
                    ranks.insert(ranker_name, rank);
                    ScoredMemory {
                        memory: mem,
                        score: contribution,
                        ranks,
                    }
                });
        }
    }

    let mut results: Vec<ScoredMemory> = acc.into_values().collect();
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(config.top_k);
    Ok(results)
}

// ------------------------------------------------------------
// 기존 Weighted 구현 (하위호환 보존)
// ------------------------------------------------------------

async fn weighted_search(
    db: &crate::memory::Db,
    query: &str,
    queries_expanded: Option<&[String]>,
    config: &HybridSearchConfig,
) -> Result<Vec<ScoredMemory>> {
    // TODO(Claude Code):
    //   기존 src/memory/search.rs 의 weighted 구현을 여기로 이관하거나
    //   delegate 하여 호출할 것. 외부 인터페이스는 동일하게 유지.
    let _ = (queries_expanded, config); // 참조용
    let embedding = embed_text(query).await?;
    let vec_hits = vector_search(db, &embedding, config.per_ranker_candidates).await?;
    let fts_hits = keyword_search_fts(db, query, config.per_ranker_candidates).await?;

    // 점수 정규화 (간이)
    let vec_max = vec_hits.iter().map(|m| m.vec_score.unwrap_or(0.0)).fold(0.0f32, f32::max).max(1e-6);
    let fts_max = fts_hits.iter().map(|m| m.fts_score.unwrap_or(0.0)).fold(0.0f32, f32::max).max(1e-6);

    let mut acc: HashMap<i64, ScoredMemory> = HashMap::new();
    for m in vec_hits {
        let s = config.vector_weight * (m.vec_score.unwrap_or(0.0) / vec_max);
        acc.insert(m.id, ScoredMemory {
            memory: m, score: s, ranks: HashMap::new(),
        });
    }
    for m in fts_hits {
        let s = config.keyword_weight * (m.fts_score.unwrap_or(0.0) / fts_max);
        acc.entry(m.id)
            .and_modify(|sm| sm.score += s)
            .or_insert(ScoredMemory { memory: m, score: s, ranks: HashMap::new() });
    }

    let mut results: Vec<ScoredMemory> = acc.into_values().collect();
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(config.top_k);
    Ok(results)
}

// ------------------------------------------------------------
// 유닛 테스트
// ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_formula_reference() {
        // rank 1 기여도 = 1/(60+1) ≈ 0.01639
        let c = HybridSearchConfig::default();
        let v1 = 1.0 / (c.rrf_k + 1.0);
        let v2 = 1.0 / (c.rrf_k + 2.0);
        assert!(v1 > v2);
        assert!((v1 - 0.01639).abs() < 1e-4);
    }

    #[test]
    fn rrf_aggregation_monotone() {
        // 동일 문서가 여러 랭커에서 상위에 등장하면 점수는 증가한다
        let base = 1.0 / (60.0 + 1.0);
        let doubled = base + 1.0 / (60.0 + 3.0);
        assert!(doubled > base);
    }

    // TODO(Claude Code): in-memory SQLite 로 E2E 테스트
    //   1) 5개 메모리 삽입
    //   2) hybrid_search(mode=Rrf) 호출
    //   3) 상위 1등이 예상대로인지 검증
}

// ============================================================
// 벤치마크 harness (별도 파일)
// benches/hybrid_search.rs 에:
//  - 변호사 질의셋 50개 (resources/eval/lawyer_queries.jsonl)
//  - 각 질의에 대한 gold memory_id 라벨
//  - weighted vs rrf recall@10 / mrr 측정
//  - 결과가 `RRF recall@10 ≥ weighted * 1.05` 이면 기본값 전환 승인
// ============================================================
