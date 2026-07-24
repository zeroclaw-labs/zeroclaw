//! Memory kind classification for consolidation writes.

use crate::consolidation::ConsolidationResult;
use crate::traits::{MemoryKind, SemanticSubtype};

/// Classify the long-lived Core write for a consolidation result.
pub fn kind_of_core(result: &ConsolidationResult) -> MemoryKind {
    let subtype = result
        .kind
        .as_deref()
        .map(parse_semantic_subtype)
        .unwrap_or(SemanticSubtype::Fact);
    MemoryKind::Semantic(subtype)
}

/// Parse a model-emitted semantic subtype, defaulting safely to Fact.
pub fn parse_semantic_subtype(raw: &str) -> SemanticSubtype {
    match raw.trim().to_ascii_lowercase().as_str() {
        "preference" | "pref" | "user_preference" => SemanticSubtype::Preference,
        "decision" | "decided" => SemanticSubtype::Decision,
        "entity" | "person" | "place" | "organization" | "org" => SemanticSubtype::Entity,
        "fact" | "" => SemanticSubtype::Fact,
        _ => SemanticSubtype::Fact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_semantic_subtype_defaults_unknown_to_fact() {
        assert_eq!(
            parse_semantic_subtype("preference"),
            SemanticSubtype::Preference
        );
        assert_eq!(parse_semantic_subtype("fact"), SemanticSubtype::Fact);
        assert_eq!(
            parse_semantic_subtype("decision"),
            SemanticSubtype::Decision
        );
        assert_eq!(parse_semantic_subtype("entity"), SemanticSubtype::Entity);
        assert_eq!(parse_semantic_subtype(""), SemanticSubtype::Fact);
        assert_eq!(parse_semantic_subtype("surprise"), SemanticSubtype::Fact);
    }

    #[test]
    fn kind_of_core_uses_result_kind_or_fact_default() {
        let decision = ConsolidationResult {
            history_entry: "Discussed rollout".into(),
            memory_update: Some("Use staged rollout".into()),
            facts: Vec::new(),
            trend: None,
            kind: Some("decision".into()),
        };
        assert_eq!(
            kind_of_core(&decision),
            MemoryKind::Semantic(SemanticSubtype::Decision)
        );

        let missing = ConsolidationResult {
            kind: None,
            ..decision
        };
        assert_eq!(
            kind_of_core(&missing),
            MemoryKind::Semantic(SemanticSubtype::Fact)
        );
    }
}
