//! Canonical slug construction for legal nodes.
//!
//! Statute article:  `statute::{법령명}::{조번호}` (sub-article joined with `-`)
//! Case:             `case::{사건번호}`
//!
//! Examples:
//!   민법 제750조        → `statute::민법::750`
//!   근로기준법 제43조의2 → `statute::근로기준법::43-2`
//!   2024노3424          → `case::2024노3424`
//!
//! Slugs are the canonical `vault_documents.title` value so the existing
//! target-lookup trigger (`SELECT id FROM vault_documents WHERE title = ?1`)
//! resolves cross-document edges deterministically. Human-readable forms
//! go into `vault_aliases`.

use regex::Regex;
use std::sync::OnceLock;

/// `43` / `43-2` / `43의2`  — accepts several input forms, returns canonical `N-M` / `N`.
pub fn article_key(num: u32, sub: Option<u32>) -> String {
    match sub {
        Some(s) if s > 0 => format!("{num}-{s}"),
        _ => num.to_string(),
    }
}

pub fn statute_slug(law_name: &str, num: u32, sub: Option<u32>) -> String {
    format!(
        "statute::{}::{}",
        law_name.trim(),
        article_key(num, sub)
    )
}

pub fn case_slug(case_number: &str) -> String {
    format!("case::{}", case_number.trim())
}

/// Slug for a statute's supplement (부칙). Keyed by the promulgation
/// number (the `법률 제N호` inside the supplement's title) so each
/// revision's 부칙 is uniquely addressable:
///
///   `statute::{법령명}::supplement::{anc_no}`
///
/// Example: `statute::근로기준법::supplement::21065` for the 부칙
/// attached to 법률 제21065호 (2025. 10. 1.) of 근로기준법.
pub fn supplement_slug(law_name: &str, promulgation_no: &str) -> String {
    format!(
        "statute::{}::supplement::{}",
        law_name.trim(),
        promulgation_no.trim()
    )
}

/// Parse a Korean article reference like `제43조의2` or `제750조` into `(num, sub)`.
/// Returns `None` for unparseable strings.
pub fn parse_article(s: &str) -> Option<(u32, Option<u32>)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"제\s*(\d+)\s*조(?:의\s*(\d+))?").unwrap());
    let caps = re.captures(s)?;
    let num: u32 = caps.get(1)?.as_str().parse().ok()?;
    let sub: Option<u32> = caps.get(2).and_then(|m| m.as_str().parse().ok());
    Some((num, sub))
}

/// Common human-readable alias forms for a statute article, for
/// `vault_aliases`. Keep conservative — aliases are UNIQUE globally so
/// collisions matter.
///
/// For every known short form of the law (from
/// `super::law_aliases::short_forms`), we also emit the shortened
/// citations (e.g. `근기법 제36조`, `근기법제36조`). This lets
/// `legal_graph_find` resolve abbreviations like "근기법 43조의2" to the
/// same article as "근로기준법 제43조의2".
pub fn statute_aliases(law_name: &str, num: u32, sub: Option<u32>) -> Vec<String> {
    let law = law_name.trim();
    let mut names: Vec<String> = vec![law.to_string()];
    for s in super::law_aliases::short_forms(law) {
        names.push(s.to_string());
    }

    let mut out = Vec::with_capacity(names.len() * 2);
    for name in names {
        match sub {
            Some(s) if s > 0 => {
                out.push(format!("{name} 제{num}조의{s}"));
                out.push(format!("{name}제{num}조의{s}"));
            }
            _ => {
                out.push(format!("{name} 제{num}조"));
                out.push(format!("{name}제{num}조"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statute_slug_basic() {
        assert_eq!(statute_slug("민법", 750, None), "statute::민법::750");
        assert_eq!(
            statute_slug("근로기준법", 43, Some(2)),
            "statute::근로기준법::43-2"
        );
    }

    #[test]
    fn case_slug_basic() {
        assert_eq!(case_slug("2024노3424"), "case::2024노3424");
    }

    #[test]
    fn parse_article_various() {
        assert_eq!(parse_article("제750조"), Some((750, None)));
        assert_eq!(parse_article("제43조의2"), Some((43, Some(2))));
        assert_eq!(parse_article("제 327 조 제 6 호"), Some((327, None)));
        assert_eq!(parse_article("없는 조문"), None);
    }

    #[test]
    fn aliases_cover_common_forms() {
        let a = statute_aliases("근로기준법", 43, Some(2));
        assert!(a.contains(&"근로기준법 제43조의2".to_string()));
        assert!(a.contains(&"근로기준법제43조의2".to_string()));
    }

    #[test]
    fn aliases_include_short_law_forms_when_known() {
        let a = statute_aliases("근로기준법", 43, Some(2));
        // `근기법` is the commonly used abbreviation.
        assert!(
            a.contains(&"근기법 제43조의2".to_string()),
            "expected 근기법 short form, got: {a:?}"
        );
        assert!(a.contains(&"근기법제43조의2".to_string()));
    }

    #[test]
    fn aliases_for_unknown_law_only_include_official_name() {
        let a = statute_aliases("존재하지않는법", 1, None);
        assert_eq!(a, vec!["존재하지않는법 제1조", "존재하지않는법제1조"]);
    }
}
