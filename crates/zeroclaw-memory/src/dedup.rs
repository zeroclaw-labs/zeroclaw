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
}
