//! Non-fatal validation warnings — config that loads and validates
//! successfully (i.e. `Config::validate()` returns `Ok(())`) but will fail
//! at agent runtime because of a logical inconsistency the schema can't
//! enforce structurally.

use serde::{Deserialize, Serialize};

/// One non-fatal validation issue surfaced after a successful save.
///
/// Stable codes (extend as new warnings are added):
/// - `memory_semantic_search_without_embedder`: `memory.search_mode` requests
///   vector search on sqlite memory, but no effective embedder is configured.
/// - `memory_config_knob_inert`: a `[memory]` knob is set to a non-default
///   value but has no runtime consumer yet, so it currently has no effect
///   (see `validate_memory_semantics` in `schema.rs` for the current list).
/// - `peer_group_channel_dangling`: a `peer_groups.<name>.channel` dotted
///   alias (`<type>.<alias>`) does not resolve to any configured
///   `[channels.<type>.<alias>]` block — typically a typo that silently
///   authorizes nobody. `Config::validate()` already hard-errors this exact
///   condition (see `classify_peer_group_channel_ref` in `schema.rs`); this
///   warning is the non-fatal surface for callers (channel doctor,
///   config-load tracing) that need the diagnostic even when a config that
///   failed `validate()` is still allowed to boot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ValidationWarning {
    /// Stable machine-readable identifier for the warning class.
    pub code: String,
    /// Human-readable description suitable for direct display.
    pub message: String,
    /// Dotted property path the warning concerns
    /// (e.g. `"agents.researcher.model_provider"`).
    pub path: String,
}

impl ValidationWarning {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: path.into(),
        }
    }
}

/// Levenshtein edit distance between `a` and `b`, operating on `char`s
/// (Unicode scalar values) rather than bytes. Used to suggest a likely
/// intended configured alias when a reference doesn't resolve — e.g. a
/// `peer_groups.*.channel` dotted alias that doesn't match any configured
/// `[channels.<type>.<alias>]` block.
pub(crate) fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    // Two-row rolling DP: `prev` holds row i-1, `curr` holds row i.
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr: Vec<usize> = vec![0; n + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1) // deletion
                .min(curr[j] + 1) // insertion
                .min(prev[j] + cost); // substitution
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Suggest the closest of `candidates` to `target` by Levenshtein distance,
/// but only when the match is close enough to plausibly be a typo of
/// `target` rather than an unrelated alias: distance <= 2, or <= one third
/// of `target`'s length (in `char`s), whichever is larger. Ties resolve to
/// whichever candidate is encountered first, so callers that want
/// deterministic output should pass `candidates` in a stable (e.g. sorted)
/// order.
///
/// Returns `None` when `target` is empty (an empty ref — e.g. a trailing-dot
/// alias like `"telegram."` — is not a typo of anything, and the distance-2
/// floor would otherwise "suggest" any alias of up to two characters), when
/// `candidates` is empty, or when nothing clears the threshold — the caller
/// should omit the suggestion in those cases rather than propose an
/// unrelated alias.
pub(crate) fn closest_match<'a>(target: &str, candidates: &'a [String]) -> Option<&'a str> {
    if target.is_empty() {
        return None;
    }
    let threshold = (target.chars().count() / 3).max(2);
    candidates
        .iter()
        .map(|candidate| (candidate.as_str(), levenshtein_distance(target, candidate)))
        .filter(|(_, distance)| *distance <= threshold)
        .min_by_key(|(_, distance)| *distance)
        .map(|(candidate, _)| candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_distance_identical_strings_is_zero() {
        assert_eq!(levenshtein_distance("alerts", "alerts"), 0);
    }

    #[test]
    fn levenshtein_distance_against_empty_string_is_length() {
        assert_eq!(levenshtein_distance("", "alerts"), 6);
        assert_eq!(levenshtein_distance("alerts", ""), 6);
    }

    #[test]
    fn levenshtein_distance_single_insertion() {
        // "alert" -> "alerts": one trailing insertion.
        assert_eq!(levenshtein_distance("alert", "alerts"), 1);
    }

    #[test]
    fn levenshtein_distance_single_substitution() {
        assert_eq!(levenshtein_distance("alerts", "alerta"), 1);
    }

    #[test]
    fn levenshtein_distance_is_symmetric() {
        assert_eq!(
            levenshtein_distance("kitten", "sitting"),
            levenshtein_distance("sitting", "kitten")
        );
        assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn levenshtein_distance_counts_unicode_scalars_not_bytes() {
        // Each of these accented characters is >1 byte in UTF-8; a
        // byte-oriented distance would over-count.
        assert_eq!(levenshtein_distance("café", "cafe"), 1);
    }

    #[test]
    fn closest_match_picks_nearest_within_threshold() {
        let candidates = vec!["alerts".to_string(), "ops".to_string()];
        assert_eq!(closest_match("alert", &candidates), Some("alerts"));
    }

    #[test]
    fn closest_match_none_when_candidates_empty() {
        let candidates: Vec<String> = Vec::new();
        assert_eq!(closest_match("alert", &candidates), None);
    }

    #[test]
    fn closest_match_none_for_empty_target() {
        // An empty target (e.g. the alias half of a trailing-dot ref like
        // "telegram.") must never suggest anything, even though short
        // candidates would clear the distance-2 floor.
        let candidates = vec!["ab".to_string(), "x".to_string()];
        assert_eq!(closest_match("", &candidates), None);
    }

    #[test]
    fn closest_match_none_when_nothing_is_a_plausible_typo() {
        // "alert" (len 5) has threshold max(5/3, 2) = 2; "ops" is distance
        // 5 away (full replace), well outside a plausible typo.
        let candidates = vec!["ops".to_string(), "workspace".to_string()];
        assert_eq!(closest_match("alert", &candidates), None);
    }

    #[test]
    fn closest_match_threshold_boundary_short_target_uses_floor_of_two() {
        // "abc" (len 3): threshold = max(3/3, 2) = max(1, 2) = 2.
        // Distance-2 candidate ("abcde", +2 insertions) must match...
        let within = vec!["abcde".to_string()];
        assert_eq!(closest_match("abc", &within), Some("abcde"));
        // ...but distance-3 must not.
        let beyond = vec!["abcdef".to_string()];
        assert_eq!(closest_match("abc", &beyond), None);
    }

    #[test]
    fn closest_match_threshold_boundary_long_target_scales_with_length() {
        // "alertchannel" (len 12): threshold = max(12/3, 2) = 4.
        // Distance-4 candidate must match...
        let within = vec!["alertchannelXXXX".to_string()];
        assert_eq!(levenshtein_distance("alertchannel", "alertchannelXXXX"), 4);
        assert_eq!(
            closest_match("alertchannel", &within),
            Some("alertchannelXXXX")
        );
        // ...but distance-5 must not.
        let beyond = vec!["alertchannelXXXXX".to_string()];
        assert_eq!(levenshtein_distance("alertchannel", "alertchannelXXXXX"), 5);
        assert_eq!(closest_match("alertchannel", &beyond), None);
    }
}
