//! Memory kind classification for consolidation writes.

use crate::consolidation::ConsolidationResult;
use crate::traits::{MemoryKind, SemanticSubtype};
use zeroclaw_api::ingress::TurnOrigin;

/// Classify the long-lived Core write for a consolidation result.
///
/// Provenance clause: the `Preference` subtype asserts "the user themself
/// expressed this preference," so it is honored only when the turn's text is
/// a person's own words ([`TurnOrigin::user_authored`]). On an autonomous
/// turn the model is summarizing its own work — a "preference" it emits
/// there is inference, not something anyone said — so the update is recorded
/// as a plain `Fact`: the content survives, the preference authority does
/// not. Every other subtype is origin-independent.
pub fn kind_of_core(result: &ConsolidationResult, origin: TurnOrigin) -> MemoryKind {
    let subtype = result
        .kind
        .as_deref()
        .map(parse_semantic_subtype)
        .unwrap_or(SemanticSubtype::Fact);
    let subtype = match subtype {
        SemanticSubtype::Preference if !origin.user_authored() => SemanticSubtype::Fact,
        honored => honored,
    };
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

    fn result_with_kind(kind: Option<&str>) -> ConsolidationResult {
        ConsolidationResult {
            history_entry: "Discussed rollout".into(),
            memory_update: Some("Use staged rollout".into()),
            facts: Vec::new(),
            trend: None,
            kind: kind.map(Into::into),
        }
    }

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
        assert_eq!(
            kind_of_core(&result_with_kind(Some("decision")), TurnOrigin::Interactive),
            MemoryKind::Semantic(SemanticSubtype::Decision)
        );
        assert_eq!(
            kind_of_core(&result_with_kind(None), TurnOrigin::Interactive),
            MemoryKind::Semantic(SemanticSubtype::Fact)
        );
    }

    #[test]
    fn preference_is_honored_when_a_person_typed_the_turn() {
        for origin in [TurnOrigin::Interactive, TurnOrigin::Channel] {
            assert_eq!(
                kind_of_core(&result_with_kind(Some("preference")), origin),
                MemoryKind::Semantic(SemanticSubtype::Preference),
                "{origin:?} carries the user's own words, so preference stands"
            );
        }
    }

    /// The provenance clause itself: an autonomous turn has no user speech
    /// in it, so a model-emitted "preference" is inference and must be
    /// recorded without preference authority.
    #[test]
    fn preference_downgrades_to_fact_on_turns_no_person_authored() {
        for origin in [
            TurnOrigin::Cron,
            TurnOrigin::Daemon,
            TurnOrigin::AgentDirect,
            TurnOrigin::SubTurn,
        ] {
            assert_eq!(
                kind_of_core(&result_with_kind(Some("preference")), origin),
                MemoryKind::Semantic(SemanticSubtype::Fact),
                "{origin:?} is not a person speaking, so preference must downgrade to fact"
            );
        }
    }

    /// Alias spellings get no side door: every token that parses to
    /// Preference downgrades the same way.
    #[test]
    fn preference_alias_spellings_downgrade_identically() {
        for spelling in ["pref", "user_preference", " Preference "] {
            assert_eq!(
                kind_of_core(&result_with_kind(Some(spelling)), TurnOrigin::Cron),
                MemoryKind::Semantic(SemanticSubtype::Fact),
                "{spelling:?} parses to Preference and must downgrade on autonomous turns"
            );
        }
    }

    /// Only preference carries a provenance claim. Facts, decisions, and
    /// entities describe the world, not the speaker, so autonomous turns
    /// keep them unchanged.
    #[test]
    fn non_preference_subtypes_are_origin_independent() {
        for origin in [
            TurnOrigin::Interactive,
            TurnOrigin::Cron,
            TurnOrigin::SubTurn,
        ] {
            assert_eq!(
                kind_of_core(&result_with_kind(Some("decision")), origin),
                MemoryKind::Semantic(SemanticSubtype::Decision)
            );
            assert_eq!(
                kind_of_core(&result_with_kind(Some("entity")), origin),
                MemoryKind::Semantic(SemanticSubtype::Entity)
            );
            assert_eq!(
                kind_of_core(&result_with_kind(Some("fact")), origin),
                MemoryKind::Semantic(SemanticSubtype::Fact)
            );
        }
    }
}
