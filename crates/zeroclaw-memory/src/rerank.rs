//! Query-time rerank machinery for recalled memory candidates.

use crate::importance::weighted_final_score;
use crate::traits::{MemoryCategory, MemoryEntry};
use chrono::{DateTime, Utc};
use std::collections::HashSet;

const DEFAULT_NEAR_DUPLICATE_THRESHOLD: f64 = 0.92;
const DEFAULT_IMPORTANCE_WEIGHT: f64 = 0.2;
const DEFAULT_RECENCY_WEIGHT: f64 = 0.1;

/// Absolute ceiling on the query-time candidate pool, enforced in `run`
/// regardless of the configured multiplier or how many entries a backend
/// returns. Bounds the O(n^2) duplicate-collapse and MMR scans against a
/// mis-sized config or a backend that ignores the requested recall limit
/// (for example a no-embedding fallback that returns its whole list).
pub const MAX_CANDIDATE_POOL: usize = 1024;

#[must_use]
pub fn bounded_final_limit(final_limit: usize) -> usize {
    final_limit.clamp(1, MAX_CANDIDATE_POOL)
}

#[must_use]
pub fn bounded_pool_cap(final_limit: usize, candidate_pool_cap: usize) -> usize {
    candidate_pool_cap
        .min(MAX_CANDIDATE_POOL)
        .max(bounded_final_limit(final_limit))
        .max(1)
}

/// Advanced rerank strategy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RerankStrategy {
    None,
    Mmr { lambda: f64 },
}

/// Rerank stage configuration, materialized from canonical memory config.
/// `Copy` so the engine's injection config (which embeds one) stays `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RerankConfig {
    pub strategy: RerankStrategy,
    pub threshold: usize,
    pub importance_weight: f64,
    pub recency_weight: f64,
    pub min_relevance_score: f64,
    pub final_limit: usize,
    /// Upper bound on the candidate pool the stage will scan, materialized
    /// from the recall limit and the configured candidate multiplier. `run`
    /// trims the incoming pool to this bound (held under `MAX_CANDIDATE_POOL`)
    /// before any blend/dedup/MMR work, so an over-returning backend cannot
    /// feed an unbounded list into the quadratic scans.
    pub candidate_pool_cap: usize,
}

impl RerankConfig {
    pub fn disabled(final_limit: usize, min_relevance_score: f64) -> Self {
        Self {
            strategy: RerankStrategy::None,
            threshold: usize::MAX,
            importance_weight: DEFAULT_IMPORTANCE_WEIGHT,
            recency_weight: DEFAULT_RECENCY_WEIGHT,
            min_relevance_score,
            final_limit: final_limit.max(1),
            candidate_pool_cap: final_limit.max(1),
        }
    }
}

/// Run blend, eligibility filtering, duplicate collapse, optional advanced
/// rerank, threshold, and trim. `is_eligible` is the caller's render-eligibility
/// predicate; it is applied before duplicate collapse so an ineligible row
/// cannot shadow an eligible fact with the same content, and before the final
/// trim so it cannot consume a selection slot.
pub fn run(
    mut pool: Vec<MemoryEntry>,
    config: &RerankConfig,
    is_eligible: impl Fn(&MemoryEntry) -> bool,
) -> Vec<MemoryEntry> {
    // Caller-side pool bound: even if a backend ignores the requested recall
    // limit and returns its whole list, never scan more than the configured
    // cap, itself held under an absolute ceiling. Applied to the raw pool in
    // backend order so a scoreless time-only recall keeps its newest-first head.
    let final_limit = bounded_final_limit(config.final_limit);
    let pool_cap = bounded_pool_cap(config.final_limit, config.candidate_pool_cap);
    if pool.len() > pool_cap {
        pool.truncate(pool_cap);
    }

    for entry in &mut pool {
        let Some(hybrid_score) = entry.score else {
            continue;
        };
        let hybrid_score = hybrid_score.clamp(0.0, 1.0);
        let importance = entry.importance.unwrap_or(0.0).clamp(0.0, 1.0);
        let recency = recency_factor(entry).clamp(0.0, 1.0);
        entry.score = Some(blended_score(
            hybrid_score,
            importance,
            recency,
            config.importance_weight,
            config.recency_weight,
        ));
    }

    // Apply the renderer's scope and hygiene boundary before duplicate
    // collapse. Otherwise a higher-scoring Conversation/autosave row can
    // discard an eligible fact with the same content, then be removed itself.
    pool.retain(|entry| is_eligible(entry));
    let mut candidates = collapse_exact_and_near_duplicates(pool, DEFAULT_NEAR_DUPLICATE_THRESHOLD);
    sort_by_score(&mut candidates);

    if candidates.len() >= config.threshold {
        candidates = match config.strategy {
            RerankStrategy::None => candidates,
            RerankStrategy::Mmr { lambda } => mmr_rerank(candidates, lambda),
        };
    }

    candidates.retain(|entry| {
        entry
            .score
            .is_none_or(|score| score >= config.min_relevance_score)
    });
    candidates.truncate(final_limit);
    candidates
}

/// Collapse exact and near-duplicate entries, preserving the highest score.
pub fn collapse_exact_and_near_duplicates(
    mut entries: Vec<MemoryEntry>,
    near_duplicate_threshold: f64,
) -> Vec<MemoryEntry> {
    sort_by_score(&mut entries);
    let mut kept: Vec<MemoryEntry> = Vec::new();
    let mut exact_contents: HashSet<String> = HashSet::new();

    'entry: for entry in entries {
        let normalized = normalize_content(&entry.content);
        if !exact_contents.insert(normalized.clone()) {
            continue;
        }
        for kept_entry in &kept {
            // Order-aware: token shingles keep directionally different facts
            // ("Alice manages Bob" vs "Bob manages Alice") apart, which an
            // unordered token-set overlap would score as identical.
            if shingle_similarity(&normalized, &normalize_content(&kept_entry.content))
                >= near_duplicate_threshold
            {
                continue 'entry;
            }
        }
        kept.push(entry);
    }

    kept
}

fn blended_score(
    hybrid_score: f64,
    importance: f64,
    recency: f64,
    importance_weight: f64,
    recency_weight: f64,
) -> f64 {
    if (importance_weight - DEFAULT_IMPORTANCE_WEIGHT).abs() < f64::EPSILON
        && (recency_weight - DEFAULT_RECENCY_WEIGHT).abs() < f64::EPSILON
    {
        return weighted_final_score(hybrid_score, importance, recency).clamp(0.0, 1.0);
    }

    let importance_weight = importance_weight.clamp(0.0, 1.0);
    let recency_weight = recency_weight.clamp(0.0, 1.0);
    let retrieval_weight = (1.0 - importance_weight - recency_weight).max(0.0);
    let total = retrieval_weight + importance_weight + recency_weight;
    if total < f64::EPSILON {
        return hybrid_score;
    }

    ((hybrid_score * retrieval_weight)
        + (importance * importance_weight)
        + (recency * recency_weight))
        / total
}

fn recency_factor(entry: &MemoryEntry) -> f64 {
    if entry.category == MemoryCategory::Core {
        return 1.0;
    }

    let Ok(timestamp) = DateTime::parse_from_rfc3339(&entry.timestamp) else {
        return 1.0;
    };
    let age_days = Utc::now()
        .signed_duration_since(timestamp.with_timezone(&Utc))
        .num_seconds()
        .max(0) as f64
        / 86_400.0;

    (-age_days / crate::decay::DEFAULT_HALF_LIFE_DAYS * std::f64::consts::LN_2).exp()
}

fn mmr_rerank(mut candidates: Vec<MemoryEntry>, lambda: f64) -> Vec<MemoryEntry> {
    if candidates.len() <= 1 {
        return candidates;
    }

    let lambda = lambda.clamp(0.0, 1.0);
    let mut selected: Vec<MemoryEntry> = Vec::with_capacity(candidates.len());

    // Seed with the relevance leader (the pool arrives sorted by score, so the
    // head is the most relevant candidate). Without this, a diversity-only
    // endpoint (`lambda = 0`) has every candidate tie at redundancy 0 on the
    // first pass, and `max_by` would return the last, least-relevant item.
    selected.push(candidates.remove(0));

    while !candidates.is_empty() {
        let best_index = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let relevance = candidate.score.unwrap_or(0.0);
                let redundancy = selected
                    .iter()
                    .map(|selected| lexical_similarity(&candidate.content, &selected.content))
                    .fold(0.0_f64, f64::max);
                let mmr_score = lambda * relevance - (1.0 - lambda) * redundancy;
                (index, mmr_score)
            })
            .max_by(|(_, left), (_, right)| {
                left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(index, _)| index)
            .unwrap_or(0);

        selected.push(candidates.remove(best_index));
    }

    selected
}

fn sort_by_score(entries: &mut [MemoryEntry]) {
    // Stable sort with no key tiebreak: entries that tie on score (notably a
    // scoreless time-only recall, where every score is `None`) keep their
    // incoming backend order instead of being reshuffled into lexicographic
    // key order, so a newest-first recall is not silently reordered.
    entries.sort_by(|left, right| {
        right
            .score
            .unwrap_or(0.0)
            .partial_cmp(&left.score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn normalize_content(content: &str) -> String {
    content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn lexical_similarity(left: &str, right: &str) -> f64 {
    let left_tokens: HashSet<&str> = left.split_whitespace().collect();
    let right_tokens: HashSet<&str> = right.split_whitespace().collect();
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let intersection = left_tokens.intersection(&right_tokens).count() as f64;
    let union = left_tokens.union(&right_tokens).count() as f64;
    if union < f64::EPSILON {
        0.0
    } else {
        intersection / union
    }
}

/// Adjacent-token (bigram) shingles of `content`. Single-token content falls
/// back to its unigram so short entries still compare.
fn token_shingles(content: &str) -> HashSet<String> {
    let tokens: Vec<&str> = content.split_whitespace().collect();
    if tokens.len() < 2 {
        return tokens.into_iter().map(str::to_string).collect();
    }
    tokens
        .windows(2)
        .map(|window| format!("{} {}", window[0], window[1]))
        .collect()
}

/// Order-aware Jaccard similarity over token bigrams. Unlike the unordered
/// token-set overlap, reversing the token order (a role-reversed fact) yields
/// disjoint shingles and a similarity of 0, so the two facts are not collapsed.
fn shingle_similarity(left: &str, right: &str) -> f64 {
    let left_shingles = token_shingles(left);
    let right_shingles = token_shingles(right);
    if left_shingles.is_empty() || right_shingles.is_empty() {
        return 0.0;
    }
    let intersection = left_shingles.intersection(&right_shingles).count() as f64;
    let union = left_shingles.union(&right_shingles).count() as f64;
    if union < f64::EPSILON {
        0.0
    } else {
        intersection / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn entry(id: &str, content: &str, score: f64, importance: f64) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            key: id.into(),
            content: content.into(),
            category: MemoryCategory::Daily,
            timestamp: Utc::now().to_rfc3339(),
            session_id: None,
            score: Some(score),
            namespace: "default".into(),
            importance: Some(importance),
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        }
    }

    fn config(strategy: RerankStrategy) -> RerankConfig {
        RerankConfig {
            strategy,
            threshold: 1,
            importance_weight: DEFAULT_IMPORTANCE_WEIGHT,
            recency_weight: DEFAULT_RECENCY_WEIGHT,
            min_relevance_score: 0.0,
            final_limit: 10,
            candidate_pool_cap: 64,
        }
    }

    /// Keep-everything eligibility predicate for the ranking-only tests.
    fn all_eligible(_entry: &MemoryEntry) -> bool {
        true
    }

    #[test]
    fn run_blends_and_sorts_with_weighted_final_score() {
        let results = run(
            vec![
                entry("low", "lower", 0.7, 0.0),
                entry("high", "higher", 0.6, 1.0),
            ],
            &config(RerankStrategy::None),
            all_eligible,
        );

        assert_eq!(results[0].key, "high");
        let expected = weighted_final_score(0.6, 1.0, 1.0);
        assert!((results[0].score.unwrap() - expected).abs() < 0.001);
    }

    #[test]
    fn threshold_applies_after_blend() {
        let mut cfg = config(RerankStrategy::None);
        cfg.min_relevance_score = 0.8;
        let results = run(
            vec![
                entry("drop", "drop this", 0.2, 0.0),
                entry("keep", "keep this", 0.9, 1.0),
            ],
            &cfg,
            all_eligible,
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "keep");
    }

    #[test]
    fn exact_duplicate_content_collapses() {
        let results = collapse_exact_and_near_duplicates(
            vec![
                entry("a", "same content", 0.9, 0.0),
                entry("b", "same   content", 0.8, 0.0),
            ],
            DEFAULT_NEAR_DUPLICATE_THRESHOLD,
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "a");
    }

    #[test]
    fn near_duplicate_content_collapses() {
        let results = collapse_exact_and_near_duplicates(
            vec![
                entry("a", "alpha beta gamma delta epsilon", 0.9, 0.0),
                entry("b", "alpha beta gamma delta zeta", 0.8, 0.0),
            ],
            0.6,
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "a");
    }

    #[test]
    fn mmr_diversifies_after_highest_relevance_item() {
        let mut cfg = config(RerankStrategy::Mmr { lambda: 0.5 });
        cfg.final_limit = 3;
        let results = run(
            vec![
                entry("a", "rust memory scoring pipeline", 1.0, 0.0),
                entry("b", "rust memory scoring pipeline duplicate", 0.98, 0.0),
                entry("c", "garden tomatoes irrigation calendar", 0.8, 0.0),
            ],
            &cfg,
            all_eligible,
        );

        assert_eq!(results[0].key, "a");
        assert_eq!(results[1].key, "c");
    }

    #[test]
    fn old_entries_get_lower_recency_signal() {
        let mut old = entry("old", "old", 1.0, 0.0);
        old.timestamp = (Utc::now() - Duration::days(7)).to_rfc3339();
        assert!(recency_factor(&old) < 0.6);
    }

    // -- review-requested regressions ------------------------------------

    /// A backend that ignores the requested limit (over-returns) must not push
    /// an unbounded pool into the quadratic scans: `run` trims the raw pool in
    /// backend order to the cap before any ranking work. An entry beyond the
    /// cap is dropped even though its score would otherwise win, proving the
    /// bound bites ahead of the scans rather than only at the final trim.
    #[test]
    fn run_bounds_oversized_candidate_pool() {
        let mut cfg = config(RerankStrategy::None);
        cfg.final_limit = 3;
        cfg.candidate_pool_cap = 3;
        let mut pool = vec![
            entry("in_a", "candidate a", 0.10, 0.0),
            entry("in_b", "candidate b", 0.20, 0.0),
            entry("in_c", "candidate c", 0.30, 0.0),
            // Position 4 is beyond the cap; its high score never gets a vote.
            entry("out_of_bound", "candidate d", 0.99, 0.0),
        ];
        for index in 0..40 {
            pool.push(entry(&format!("filler{index:02}"), "filler", 0.05, 0.0));
        }

        let results = run(pool, &cfg, all_eligible);

        assert_eq!(results.len(), 3, "output bounded by the pool cap");
        assert!(
            !results.iter().any(|entry| entry.key == "out_of_bound"),
            "entry beyond the pool cap is truncated before ranking"
        );
    }

    /// Role-reversed facts share every token but no ordered bigram, so the
    /// near-duplicate pass must keep both instead of discarding one.
    #[test]
    fn role_reversed_facts_are_not_collapsed() {
        let results = collapse_exact_and_near_duplicates(
            vec![
                entry("fwd", "alice manages bob", 0.9, 0.0),
                entry("rev", "bob manages alice", 0.8, 0.0),
            ],
            DEFAULT_NEAR_DUPLICATE_THRESHOLD,
        );

        assert_eq!(results.len(), 2, "reversed relationship must survive");
        assert!(results.iter().any(|entry| entry.key == "fwd"));
        assert!(results.iter().any(|entry| entry.key == "rev"));
    }

    /// A scoreless time-only recall arrives newest-first; the stage must not
    /// reorder it into lexicographic key order, which would discard the newest
    /// entries at the final trim.
    #[test]
    fn scoreless_entries_preserve_backend_order() {
        // Keys are reverse-sorted against backend (newest-first) order, so key
        // ordering and backend ordering disagree.
        let mut newest = entry("z_newest", "newest fact", 0.0, 0.0);
        newest.score = None;
        let mut middle = entry("m_middle", "middle fact", 0.0, 0.0);
        middle.score = None;
        let mut oldest = entry("a_oldest", "oldest fact", 0.0, 0.0);
        oldest.score = None;

        let mut cfg = config(RerankStrategy::None);
        cfg.final_limit = 2;
        let results = run(vec![newest, middle, oldest], &cfg, all_eligible);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].key, "z_newest", "newest stays first");
        assert_eq!(results[1].key, "m_middle");
    }

    /// Ineligible rows are removed after ranking but before the final trim, so
    /// eligible over-fetched candidates below the cutoff fill the freed slots.
    #[test]
    fn ineligible_entries_filtered_before_final_trim() {
        let mut cfg = config(RerankStrategy::None);
        cfg.final_limit = 2;
        // "skip_top" outranks everything but is ineligible; without the
        // pre-trim filter it would occupy a final slot and shrink the output.
        let results = run(
            vec![
                entry("skip_top", "ineligible leader", 0.99, 0.0),
                entry("keep_a", "first eligible", 0.9, 0.0),
                entry("keep_b", "second eligible", 0.8, 0.0),
            ],
            &cfg,
            |entry| entry.key != "skip_top",
        );

        assert_eq!(results.len(), 2, "eligible candidate fills the freed slot");
        assert_eq!(results[0].key, "keep_a");
        assert_eq!(results[1].key, "keep_b");
    }

    /// An ineligible row with duplicate content must not shadow an eligible
    /// fact during collapse and then disappear at the render boundary.
    #[test]
    fn ineligible_duplicate_cannot_collapse_eligible_fact() {
        let mut cfg = config(RerankStrategy::None);
        cfg.final_limit = 1;
        let results = run(
            vec![
                entry(
                    "skip_autosave",
                    "the release train leaves Friday",
                    0.99,
                    0.0,
                ),
                entry("curated_fact", "the release train leaves Friday", 0.8, 0.0),
            ],
            &cfg,
            |entry| entry.key != "skip_autosave",
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "curated_fact");
    }

    /// Diversity-only endpoint: every candidate ties on the first MMR pass, so
    /// the seed must be the relevance leader, not an arbitrary tail item.
    #[test]
    fn mmr_lambda_zero_keeps_relevance_leader_first() {
        let mut cfg = config(RerankStrategy::Mmr { lambda: 0.0 });
        cfg.final_limit = 1;
        cfg.threshold = 1;
        let results = run(
            vec![
                entry("leader", "top relevance fact", 0.95, 0.0),
                entry("mid", "middle relevance fact", 0.6, 0.0),
                entry("tail", "low relevance fact", 0.2, 0.0),
            ],
            &cfg,
            all_eligible,
        );

        assert_eq!(results[0].key, "leader");
    }

    /// Relevance-only endpoint: `lambda = 1` collapses MMR to pure relevance
    /// order, leader first.
    #[test]
    fn mmr_lambda_one_is_pure_relevance() {
        let mut cfg = config(RerankStrategy::Mmr { lambda: 1.0 });
        cfg.final_limit = 3;
        cfg.threshold = 1;
        let results = run(
            vec![
                entry("mid", "middle relevance fact", 0.6, 0.0),
                entry("leader", "top relevance fact", 0.95, 0.0),
                entry("tail", "low relevance fact", 0.2, 0.0),
            ],
            &cfg,
            all_eligible,
        );

        assert_eq!(results[0].key, "leader");
        assert_eq!(results[1].key, "mid");
        assert_eq!(results[2].key, "tail");
    }
}
