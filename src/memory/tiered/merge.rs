//! Recall merge: weighted scoring, deduplication, threshold filtering, diversity guard.

use crate::memory::tiered::types::MemoryTier;
use std::collections::HashSet;

// ── TieredRecallItem ──────────────────────────────────────────────────────────

/// A single candidate result from any tier, ready to be merged and ranked.
#[derive(Debug, Clone, PartialEq)]
pub struct TieredRecallItem {
    pub entry_id: String,
    /// Root lineage ID for deduplication. For raw entries = entry_id.
    /// For compressed entries, this points to the original source.
    pub origin_id: String,
    pub tier: MemoryTier,
    /// Relevance score from backend, normalized 0.0–1.0
    pub base_score: f32,
    /// How recent: 1.0 = just now, 0.0 = 24h+ ago
    pub recency_score: f32,
    /// True if an STM IndexEntry explicitly references this MTM/LTM entry
    pub has_cross_tier_link: bool,
    /// Computed by merge_and_rank(), 0.0 before that
    pub final_score: f32,
}

// ── TierWeights ───────────────────────────────────────────────────────────────

/// Per-tier multipliers applied to base_score during ranking.
#[derive(Debug, Clone, Copy)]
pub struct TierWeights {
    pub stm: f32,
    pub mtm: f32,
    pub ltm: f32,
}

impl TierWeights {
    /// Returns the weight for a given tier.
    pub fn weight_for(&self, tier: MemoryTier) -> f32 {
        match tier {
            MemoryTier::Stm => self.stm,
            MemoryTier::Mtm => self.mtm,
            MemoryTier::Ltm => self.ltm,
        }
    }
}

// ── merge_and_rank ────────────────────────────────────────────────────────────

/// Merge candidates from all tiers into a ranked, deduplicated, diverse list.
///
/// Scoring formula:
///   final_score = tier_weight * base_score + 0.25 * recency_score + link_boost
///   where link_boost = 0.15 if has_cross_tier_link else 0.0
///
/// Deduplication: keep highest final_score per origin_id
/// Threshold: filter items where final_score < min_threshold
/// Diversity guard: reserve 1 slot each for STM and MTM if available
/// Returns items in descending `final_score` order.
pub fn merge_and_rank(
    items: Vec<TieredRecallItem>,
    weights: &TierWeights,
    min_threshold: f32,
    top_k: usize,
) -> Vec<TieredRecallItem> {
    // 1. Score all items.
    let mut scored: Vec<TieredRecallItem> = items
        .into_iter()
        .map(|mut item| {
            let link_boost = if item.has_cross_tier_link { 0.15 } else { 0.0 };
            item.final_score = weights.weight_for(item.tier) * item.base_score
                + 0.25 * item.recency_score
                + link_boost;
            item
        })
        .collect();

    // 2. Sort descending by final_score so dedup keeps the best per origin.
    scored.sort_by(|a, b| b.final_score.total_cmp(&a.final_score));

    // 3. Deduplicate: keep first (highest-scored) occurrence per origin_id.
    let mut seen_origins: HashSet<String> = HashSet::new();
    let deduped: Vec<TieredRecallItem> = scored
        .into_iter()
        .filter(|item| seen_origins.insert(item.origin_id.clone()))
        .collect();

    // 4. Apply minimum threshold filter.
    let filtered: Vec<TieredRecallItem> = deduped
        .into_iter()
        .filter(|item| item.final_score >= min_threshold)
        .collect();

    // 5. Apply diversity guard and take top-k.
    apply_diversity_guard(filtered, top_k)
}

/// Reserve 1 slot for the highest-scoring STM candidate and 1 for the highest-scoring MTM
/// candidate (when available), then fill remaining slots with the top scorers from the rest,
/// then re-sort and truncate to top_k.
fn apply_diversity_guard(
    items: Vec<TieredRecallItem>,
    top_k: usize,
) -> Vec<TieredRecallItem> {
    if top_k == 0 {
        return Vec::new();
    }

    // Items are already sorted descending by final_score at this point.
    // Find the best STM candidate and the best MTM candidate.
    let stm_candidate = items.iter().find(|i| i.tier == MemoryTier::Stm).cloned();
    let mtm_candidate = items.iter().find(|i| i.tier == MemoryTier::Mtm).cloned();

    // Track which entry_ids are reserved so we can exclude them from the general pool.
    let mut reserved_ids: HashSet<String> = HashSet::new();
    if let Some(ref s) = stm_candidate {
        reserved_ids.insert(s.entry_id.clone());
    }
    if let Some(ref m) = mtm_candidate {
        reserved_ids.insert(m.entry_id.clone());
    }

    let reserved_count = stm_candidate.is_some() as usize + mtm_candidate.is_some() as usize;

    // Build the general pool: everything that is NOT a reserved candidate.
    let remaining: Vec<TieredRecallItem> = items
        .into_iter()
        .filter(|i| !reserved_ids.contains(&i.entry_id))
        .collect();

    // Slots available for the general pool (saturating so we never underflow).
    let pool_slots = top_k.saturating_sub(reserved_count);

    // Take the best of the general pool.
    let mut result: Vec<TieredRecallItem> = remaining.into_iter().take(pool_slots).collect();

    // Append the reserved candidates.
    if let Some(s) = stm_candidate {
        result.push(s);
    }
    if let Some(m) = mtm_candidate {
        result.push(m);
    }

    // Re-sort and truncate to top_k.
    result.sort_by(|a, b| b.final_score.total_cmp(&a.final_score));
    result.truncate(top_k);
    result
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::tiered::types::MemoryTier;

    fn make_item(id: &str, tier: MemoryTier, base: f32, age_secs: i64) -> TieredRecallItem {
        TieredRecallItem {
            entry_id: id.to_string(),
            origin_id: id.to_string(),
            tier,
            base_score: base,
            recency_score: 1.0 - (age_secs as f32 / 86400.0).min(1.0),
            has_cross_tier_link: false,
            final_score: 0.0,
        }
    }

    #[test]
    fn merge_deduplicates_by_origin_id() {
        // "a" appears in STM and LTM with same origin_id — only the higher-scoring one survives
        let items = vec![
            make_item("a", MemoryTier::Stm, 0.9, 100),
            make_item("a", MemoryTier::Ltm, 0.8, 1000),
            make_item("b", MemoryTier::Mtm, 0.7, 500),
        ];
        let weights = TierWeights { stm: 0.45, mtm: 0.35, ltm: 0.20 };
        let merged = merge_and_rank(items, &weights, 0.0, 5);
        assert_eq!(merged.len(), 2, "should deduplicate 'a'");
        // STM "a" should win (higher weight * base)
        assert_eq!(merged[0].tier, MemoryTier::Stm);
    }

    #[test]
    fn merge_applies_relevance_threshold() {
        let items = vec![
            make_item("high", MemoryTier::Stm, 0.9, 100),
            make_item("low", MemoryTier::Ltm, 0.05, 100),
        ];
        let weights = TierWeights { stm: 0.45, mtm: 0.35, ltm: 0.20 };
        let merged = merge_and_rank(items, &weights, 0.4, 5);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].entry_id, "high");
    }

    #[test]
    fn merge_respects_top_k() {
        let items: Vec<_> = (0..10)
            .map(|i| make_item(&i.to_string(), MemoryTier::Stm, 0.9 - i as f32 * 0.05, 100))
            .collect();
        let weights = TierWeights { stm: 0.45, mtm: 0.35, ltm: 0.20 };
        let merged = merge_and_rank(items, &weights, 0.0, 3);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn diversity_guard_includes_one_stm_and_one_mtm() {
        // 5 high-scoring LTM items, 1 low-scoring STM, 1 low-scoring MTM
        // Diversity guard must include at least one STM and one MTM in top 5
        let mut items: Vec<_> = (0..5)
            .map(|i| make_item(&format!("ltm-{}", i), MemoryTier::Ltm, 0.95, 1000))
            .collect();
        items.push(make_item("stm-1", MemoryTier::Stm, 0.5, 50));
        items.push(make_item("mtm-1", MemoryTier::Mtm, 0.5, 200));
        let weights = TierWeights { stm: 0.45, mtm: 0.35, ltm: 0.20 };
        let merged = merge_and_rank(items, &weights, 0.0, 5);
        assert!(merged.iter().any(|i| i.tier == MemoryTier::Stm), "must include STM");
        assert!(merged.iter().any(|i| i.tier == MemoryTier::Mtm), "must include MTM");
    }

    #[test]
    fn cross_tier_link_boost_raises_score() {
        // Two items with same base score; one has cross-tier link. Linked one should rank higher.
        let mut boosted = make_item("boosted", MemoryTier::Ltm, 0.5, 100);
        boosted.has_cross_tier_link = true;
        let normal = make_item("normal", MemoryTier::Ltm, 0.5, 100);
        let weights = TierWeights { stm: 0.45, mtm: 0.35, ltm: 0.20 };
        let merged = merge_and_rank(vec![normal, boosted], &weights, 0.0, 5);
        assert_eq!(merged[0].entry_id, "boosted");
    }

    #[test]
    fn merge_handles_nan_scores_without_panic() {
        let mut nan_item = make_item("nan", MemoryTier::Stm, f32::NAN, 100);
        nan_item.base_score = f32::NAN;
        let normal = make_item("normal", MemoryTier::Ltm, 0.8, 100);
        let weights = TierWeights { stm: 0.45, mtm: 0.35, ltm: 0.20 };
        // Should not panic
        let result = merge_and_rank(vec![nan_item, normal], &weights, 0.0, 5);
        assert!(!result.is_empty());
    }
}
