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
        // Default config has daily_dedup on.
        let cfg = MemoryConfig::default();
        assert!(cfg.daily_dedup);
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
        let cfg = MemoryConfig::default();
        assert_eq!(
            should_write_daily(&[], "   ", &cfg),
            DailyWrite::Skip {
                dup_of: String::new()
            }
        );
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
