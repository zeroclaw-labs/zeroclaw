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
}
