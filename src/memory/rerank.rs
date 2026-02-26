use super::vector::{cosine_similarity, ScoredResult};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct MmrConfig {
    pub lambda: f64,
    pub enabled: bool,
}

impl Default for MmrConfig {
    fn default() -> Self {
        Self {
            lambda: 0.7,
            enabled: false,
        }
    }
}

pub fn mmr_rerank<S: std::hash::BuildHasher>(
    candidates: &[ScoredResult],
    query_embedding: &[f32],
    embeddings: &HashMap<String, Vec<f32>, S>,
    lambda: f64,
    limit: usize,
) -> Vec<ScoredResult> {
    if candidates.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut remaining: Vec<usize> = (0..candidates.len()).collect();
    let mut selected: Vec<usize> = Vec::with_capacity(limit.min(candidates.len()));

    while selected.len() < limit && !remaining.is_empty() {
        let mut best_idx_in_remaining = 0;
        let mut best_mmr = f64::NEG_INFINITY;

        for (ri, &ci) in remaining.iter().enumerate() {
            let candidate = &candidates[ci];

            let relevance = match embeddings.get(&candidate.id) {
                Some(emb) => f64::from(cosine_similarity(emb, query_embedding)),
                None => f64::from(candidate.final_score),
            };

            let max_sim = if selected.is_empty() {
                0.0
            } else {
                selected
                    .iter()
                    .filter_map(|&si| {
                        let sel = &candidates[si];
                        let c_emb = embeddings.get(&candidate.id)?;
                        let s_emb = embeddings.get(&sel.id)?;
                        Some(f64::from(cosine_similarity(c_emb, s_emb)))
                    })
                    .fold(0.0_f64, f64::max)
            };

            let mmr_score = lambda * relevance - (1.0 - lambda) * max_sim;

            if mmr_score > best_mmr {
                best_mmr = mmr_score;
                best_idx_in_remaining = ri;
            }
        }

        let chosen = remaining.swap_remove(best_idx_in_remaining);
        selected.push(chosen);
    }

    selected
        .into_iter()
        .map(|i| candidates[i].clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_candidate(id: &str, score: f32) -> ScoredResult {
        ScoredResult {
            id: id.to_string(),
            vector_score: Some(score),
            keyword_score: None,
            final_score: score,
        }
    }

    fn make_embeddings(pairs: &[(&str, Vec<f32>)]) -> HashMap<String, Vec<f32>> {
        pairs
            .iter()
            .map(|(id, emb)| (id.to_string(), emb.clone()))
            .collect()
    }

    #[test]
    fn mmr_empty_candidates_returns_empty() {
        let result = mmr_rerank(&[], &[1.0, 0.0], &HashMap::new(), 0.7, 5);
        assert!(result.is_empty());
    }

    #[test]
    fn mmr_limit_zero_returns_empty() {
        let candidates = vec![make_candidate("a", 0.9)];
        let embs = make_embeddings(&[("a", vec![1.0, 0.0])]);
        let result = mmr_rerank(&candidates, &[1.0, 0.0], &embs, 0.7, 0);
        assert!(result.is_empty());
    }

    #[test]
    fn mmr_single_candidate() {
        let candidates = vec![make_candidate("a", 0.9)];
        let embs = make_embeddings(&[("a", vec![1.0, 0.0])]);
        let result = mmr_rerank(&candidates, &[1.0, 0.0], &embs, 0.7, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn mmr_lambda_one_preserves_relevance_order() {
        let candidates = vec![
            make_candidate("a", 0.9),
            make_candidate("b", 0.7),
            make_candidate("c", 0.5),
        ];
        let embs = make_embeddings(&[
            ("a", vec![0.9, 0.1, 0.0]),
            ("b", vec![0.7, 0.3, 0.0]),
            ("c", vec![0.0, 0.0, 1.0]),
        ]);
        let query = vec![1.0, 0.0, 0.0];
        let result = mmr_rerank(&candidates, &query, &embs, 1.0, 3);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].id, "a");
        assert_eq!(result[1].id, "b");
    }

    #[test]
    fn mmr_lambda_zero_maximizes_diversity() {
        let candidates = vec![
            make_candidate("a", 0.95),
            make_candidate("a_dup", 0.90),
            make_candidate("diverse", 0.50),
        ];
        let embs = make_embeddings(&[
            ("a", vec![1.0, 0.0, 0.0]),
            ("a_dup", vec![0.99, 0.01, 0.0]),
            ("diverse", vec![0.0, 0.0, 1.0]),
        ]);
        let query = vec![1.0, 0.0, 0.0];
        let result = mmr_rerank(&candidates, &query, &embs, 0.0, 2);
        assert_eq!(result.len(), 2);
        let ids: Vec<&str> = result.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"diverse"),
            "Diverse candidate should be selected with lambda=0.0, got: {ids:?}"
        );
    }

    #[test]
    fn mmr_near_duplicates_deprioritized() {
        let candidates = vec![
            make_candidate("orig", 0.9),
            make_candidate("dup1", 0.89),
            make_candidate("dup2", 0.88),
            make_candidate("different", 0.6),
        ];
        let base = vec![1.0, 0.0, 0.0];
        let embs = make_embeddings(&[
            ("orig", base.clone()),
            ("dup1", vec![0.999, 0.001, 0.0]),
            ("dup2", vec![0.998, 0.002, 0.0]),
            ("different", vec![0.0, 1.0, 0.0]),
        ]);
        let query = vec![1.0, 0.0, 0.0];
        let result = mmr_rerank(&candidates, &query, &embs, 0.5, 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "orig");
        assert_eq!(
            result[1].id, "different",
            "With lambda=0.5, diversity should push 'different' above near-duplicates"
        );
    }

    #[test]
    fn mmr_limit_greater_than_candidates() {
        let candidates = vec![make_candidate("a", 0.9), make_candidate("b", 0.5)];
        let embs = make_embeddings(&[("a", vec![1.0, 0.0]), ("b", vec![0.0, 1.0])]);
        let result = mmr_rerank(&candidates, &[1.0, 0.0], &embs, 0.7, 10);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn mmr_candidates_without_embeddings_use_final_score() {
        let candidates = vec![make_candidate("a", 0.9), make_candidate("no_emb", 0.7)];
        let embs = make_embeddings(&[("a", vec![1.0, 0.0])]);
        let result = mmr_rerank(&candidates, &[1.0, 0.0], &embs, 0.7, 2);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn mmr_balanced_lambda_reasonable_mix() {
        let candidates = vec![
            make_candidate("topic_a", 0.95),
            make_candidate("topic_b", 0.70),
            make_candidate("topic_a_dup", 0.93),
        ];
        let embs = make_embeddings(&[
            ("topic_a", vec![0.95, 0.31, 0.0]),
            ("topic_b", vec![0.31, 0.95, 0.0]),
            ("topic_a_dup", vec![0.96, 0.28, 0.0]),
        ]);
        let query = vec![0.8, 0.6, 0.0];
        let result = mmr_rerank(&candidates, &query, &embs, 0.5, 3);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].id, "topic_a");
        assert_eq!(
            result[1].id, "topic_b",
            "With lambda=0.5, topic_b should beat topic_a_dup due to diversity"
        );
    }
}
