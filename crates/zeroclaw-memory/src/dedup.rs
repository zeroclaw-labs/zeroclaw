use crate::conflict;
use crate::traits::{MemoryCategory, MemoryEntry};
use zeroclaw_config::schema::{MemoryConfig, MemoryDedupAction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupAction {
    Insert,
    Reject { dup_of: String },
    Merge { into: String },
}

pub fn dedup_gate(candidates: &[MemoryEntry], incoming: &str, cfg: &MemoryConfig) -> DedupAction {
    if !cfg.dedup_on_write {
        return DedupAction::Insert;
    }

    if let Some(existing) = candidates.iter().find(|entry| entry.content == incoming) {
        return match cfg.dedup_action {
            MemoryDedupAction::Reject => DedupAction::Reject {
                dup_of: existing.id.clone(),
            },
            MemoryDedupAction::Merge => DedupAction::Merge {
                into: existing.id.clone(),
            },
        };
    }

    let Some(dup_of) =
        conflict::find_text_conflicts(candidates, incoming, cfg.dedup_jaccard_threshold)
            .into_iter()
            .next()
    else {
        return DedupAction::Insert;
    };

    match cfg.dedup_action {
        MemoryDedupAction::Reject => DedupAction::Reject { dup_of },
        MemoryDedupAction::Merge => DedupAction::Merge { into: dup_of },
    }
}

pub fn core_candidates(entries: Vec<MemoryEntry>) -> Vec<MemoryEntry> {
    entries
        .into_iter()
        .filter(|entry| {
            matches!(entry.category, MemoryCategory::Core) && entry.superseded_by.is_none()
        })
        .collect()
}

/// Keep only Daily, non-superseded entries as dedup candidates for the per-turn
/// history write.
pub fn daily_candidates(entries: Vec<MemoryEntry>) -> Vec<MemoryEntry> {
    entries
        .into_iter()
        .filter(|entry| {
            matches!(entry.category, MemoryCategory::Daily) && entry.superseded_by.is_none()
        })
        .collect()
}

/// Decide whether a new per-turn Daily history summary should be written.
///
/// Independent of `dedup_on_write` (which governs only the Phase-2 Core write):
/// this is gated by [`MemoryConfig::daily_dedup`]. When dedup is off, always
/// write. When on, skip the write if an existing Daily candidate is an exact
/// match or exceeds the configured Jaccard similarity threshold - so repeated
/// or near-identical turn summaries do not accumulate transient rows. `id` of
/// the matched entry is returned for logging.
pub fn should_write_daily(
    candidates: &[MemoryEntry],
    incoming: &str,
    cfg: &MemoryConfig,
) -> DailyWrite {
    if !cfg.daily_dedup {
        return DailyWrite::Insert;
    }
    // Never store an empty/whitespace-only summary.
    if incoming.trim().is_empty() {
        return DailyWrite::Skip {
            dup_of: String::new(),
        };
    }
    if let Some(existing) = candidates.iter().find(|entry| entry.content == incoming) {
        return DailyWrite::Skip {
            dup_of: existing.id.clone(),
        };
    }
    match conflict::find_similar(candidates, incoming, cfg.dedup_jaccard_threshold)
        .into_iter()
        .next()
    {
        Some(dup_of) => DailyWrite::Skip { dup_of },
        None => DailyWrite::Insert,
    }
}

/// In-turn cross-path check: does this turn's Daily `history_entry` say the same
/// thing as the SAME turn's Core `memory_update`?
///
/// Reuses the existing Jaccard similarity ([`conflict::jaccard_similarity`]) and
/// the same `dedup_jaccard_threshold` used by the Daily/Core dedup gates - no new
/// similarity function and no new knob. Gated by [`MemoryConfig::daily_dedup`]
/// (the existing "don't flood Daily" switch): when it is off, this never skips.
/// Blank sides never match. This closes the gap where a fact was stored once as a
/// durable Core row and again as a transient Daily row (there was no
/// cross-category comparison), so recall surfaced it twice.
pub fn daily_duplicates_core(history_entry: &str, core_update: &str, cfg: &MemoryConfig) -> bool {
    if !cfg.daily_dedup {
        return false;
    }
    let history = history_entry.trim();
    let core = core_update.trim();
    if history.is_empty() || core.is_empty() {
        return false;
    }
    history == core || conflict::jaccard_similarity(history, core) >= cfg.dedup_jaccard_threshold
}

/// Outcome of the per-turn Daily dedup gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DailyWrite {
    /// Write the new Daily history entry.
    Insert,
    /// Skip the write; `dup_of` is the id of the near-duplicate (empty when the
    /// summary was blank).
    Skip { dup_of: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryCategory;

    fn entry(id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            key: id.into(),
            content: content.into(),
            category: MemoryCategory::Core,
            timestamp: "now".into(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance: Some(0.7),
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        }
    }

    #[test]
    fn disabled_gate_inserts() {
        let cfg = MemoryConfig::default();
        let action = dedup_gate(
            &[entry("a", "User prefers Rust")],
            "User prefers Rust",
            &cfg,
        );
        assert_eq!(action, DedupAction::Insert);
    }

    #[test]
    fn enabled_gate_rejects_near_duplicate() {
        let cfg = MemoryConfig {
            dedup_on_write: true,
            dedup_jaccard_threshold: 0.5,
            ..MemoryConfig::default()
        };
        let action = dedup_gate(
            &[entry("a", "User prefers Rust for systems work")],
            "User prefers Rust for systems work",
            &cfg,
        );
        assert_eq!(action, DedupAction::Reject { dup_of: "a".into() });
    }

    #[test]
    fn near_duplicate_above_threshold_rejects_via_jaccard() {
        let cfg = MemoryConfig {
            dedup_on_write: true,
            dedup_jaccard_threshold: 0.5,
            ..MemoryConfig::default()
        };
        // Word sets {alpha,beta,gamma} vs {alpha,beta,gamma,delta}:
        // jaccard = 3/4 = 0.75 > 0.5, and the contents are not identical,
        // so this exercises the similarity branch, not the exact-match one.
        let action = dedup_gate(
            &[entry("a", "alpha beta gamma")],
            "alpha beta gamma delta",
            &cfg,
        );
        assert_eq!(action, DedupAction::Reject { dup_of: "a".into() });
    }

    #[test]
    fn jaccard_at_exact_threshold_inserts() {
        let cfg = MemoryConfig {
            dedup_on_write: true,
            dedup_jaccard_threshold: 0.5,
            ..MemoryConfig::default()
        };
        // Word sets {alpha,beta} vs {alpha,beta,gamma,delta}:
        // jaccard = 2/4 = 0.5 exactly. The gate requires similarity
        // strictly above the threshold, so the boundary inserts.
        let action = dedup_gate(&[entry("a", "alpha beta")], "alpha beta gamma delta", &cfg);
        assert_eq!(action, DedupAction::Insert);
    }

    #[test]
    fn below_threshold_inserts() {
        let cfg = MemoryConfig {
            dedup_on_write: true,
            dedup_jaccard_threshold: 0.5,
            ..MemoryConfig::default()
        };
        // jaccard = 1/3, well under the threshold.
        let action = dedup_gate(&[entry("a", "alpha beta")], "alpha gamma", &cfg);
        assert_eq!(action, DedupAction::Insert);
    }

    #[test]
    fn merge_action_targets_the_survivor() {
        let cfg = MemoryConfig {
            dedup_on_write: true,
            dedup_jaccard_threshold: 0.5,
            dedup_action: MemoryDedupAction::Merge,
            ..MemoryConfig::default()
        };
        let action = dedup_gate(
            &[entry("a", "alpha beta gamma")],
            "alpha beta gamma delta",
            &cfg,
        );
        assert_eq!(action, DedupAction::Merge { into: "a".into() });
    }

    #[test]
    fn exact_duplicate_short_circuits_the_jaccard_threshold() {
        let cfg = MemoryConfig {
            dedup_on_write: true,
            // Jaccard can never be strictly greater than 1.0, so only the
            // exact-content branch can fire under this config.
            dedup_jaccard_threshold: 1.0,
            ..MemoryConfig::default()
        };
        let action = dedup_gate(&[entry("a", "alpha beta")], "alpha beta", &cfg);
        assert_eq!(action, DedupAction::Reject { dup_of: "a".into() });
    }

    #[test]
    fn core_candidates_keeps_only_live_core_entries() {
        let live_core = entry("live", "kept");
        let mut superseded_core = entry("superseded", "hidden");
        superseded_core.superseded_by = Some("live".into());
        let mut daily = entry("daily", "log line");
        daily.category = MemoryCategory::Daily;

        let filtered = core_candidates(vec![live_core, superseded_core, daily]);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "live");
    }

    fn daily_entry(id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            category: MemoryCategory::Daily,
            ..entry(id, content)
        }
    }

    #[test]
    fn daily_gate_off_always_inserts() {
        // With daily_dedup off, even an exact duplicate is inserted (the Daily
        // write is unconditional, as before this fix).
        let cfg = MemoryConfig {
            daily_dedup: false,
            ..MemoryConfig::default()
        };
        let action = should_write_daily(
            &[daily_entry("a", "Discussed the deploy plan")],
            "Discussed the deploy plan",
            &cfg,
        );
        assert_eq!(action, DailyWrite::Insert);
    }

    #[test]
    fn daily_gate_skips_exact_duplicate() {
        // daily_dedup is opt-in (default off); enable it explicitly.
        let cfg = MemoryConfig {
            daily_dedup: true,
            ..MemoryConfig::default()
        };
        let action = should_write_daily(
            &[daily_entry("a", "Discussed the deploy plan")],
            "Discussed the deploy plan",
            &cfg,
        );
        assert_eq!(action, DailyWrite::Skip { dup_of: "a".into() });
    }

    #[test]
    fn daily_gate_skips_near_duplicate() {
        let cfg = MemoryConfig {
            daily_dedup: true,
            dedup_jaccard_threshold: 0.5,
            ..MemoryConfig::default()
        };
        // Same topic, one extra word: high token overlap -> treated as dup.
        let action = should_write_daily(
            &[daily_entry("a", "User asked about the weather today")],
            "User asked about the weather",
            &cfg,
        );
        assert_eq!(action, DailyWrite::Skip { dup_of: "a".into() });
    }

    #[test]
    fn daily_gate_inserts_novel_summary() {
        let cfg = MemoryConfig::default();
        let action = should_write_daily(
            &[daily_entry("a", "User asked about the weather")],
            "User configured a new Postgres backend",
            &cfg,
        );
        assert_eq!(action, DailyWrite::Insert);
    }

    #[test]
    fn daily_gate_skips_blank_summary() {
        let cfg = MemoryConfig {
            daily_dedup: true,
            ..MemoryConfig::default()
        };
        assert_eq!(
            should_write_daily(&[], "   ", &cfg),
            DailyWrite::Skip {
                dup_of: String::new()
            }
        );
    }

    #[test]
    fn daily_duplicates_core_flags_near_identical_paraphrase() {
        let cfg = MemoryConfig {
            daily_dedup: true,
            ..MemoryConfig::default()
        };
        // Same fact, reworded: high token overlap -> treated as a cross-path dup.
        let core = "relative_a is user_a's mentor and advisor to user_b and user_c";
        let daily = "relative_a is user_a's mentor and also advisor to user_b and user_c";
        assert!(daily_duplicates_core(daily, core, &cfg));
    }

    #[test]
    fn daily_duplicates_core_ignores_unrelated_fact() {
        // Enable the gate explicitly so this exercises the "unrelated" branch
        // rather than trivially passing via the default-off short circuit.
        let cfg = MemoryConfig {
            daily_dedup: true,
            ..MemoryConfig::default()
        };
        assert!(!daily_duplicates_core(
            "User asked what the weather is like today",
            "user_a prefers the metric system for unit conversions",
            &cfg,
        ));
    }

    #[test]
    fn daily_duplicates_core_off_when_daily_dedup_disabled() {
        let cfg = MemoryConfig {
            daily_dedup: false,
            ..MemoryConfig::default()
        };
        // Even an exact match is not flagged when the gate is off.
        assert!(!daily_duplicates_core(
            "same fact here",
            "same fact here",
            &cfg
        ));
    }

    #[test]
    fn daily_duplicates_core_ignores_blank_sides() {
        let cfg = MemoryConfig::default();
        assert!(!daily_duplicates_core("   ", "a real core fact", &cfg));
        assert!(!daily_duplicates_core("a real daily summary", "", &cfg));
    }

    #[test]
    fn daily_candidates_filters_to_daily_non_superseded() {
        let mut core = entry("c", "core fact");
        core.category = MemoryCategory::Core;
        let mut superseded = daily_entry("s", "old daily");
        superseded.superseded_by = Some("x".into());
        let keep = daily_entry("d", "kept daily");
        let out = daily_candidates(vec![core, superseded, keep]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "d");
    }
}
