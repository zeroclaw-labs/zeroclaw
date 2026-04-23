//! Deterministic regex-based citation extraction for Korean legal text.
//!
//! Two entry points:
//!   - [`extract_statute_citations`] — scans a block of text for statute refs
//!     with context-carrying "same-law" inheritance (a bare `제36조` after
//!     `근로기준법 제43조` inherits `근로기준법`). This is the behavior of
//!     판례 참조조문 blocks like
//!     `형사소송법 제327조 제6호, 제364조 제6항, 근로기준법 제36조, 제109조`.
//!   - [`extract_case_numbers`] — scans text for Korean case numbers like
//!     `2024노3424`, `2012도3166`, `2023가단92476`.
//!
//! **Non-goals**: fuzzy LLM extraction, historical-version resolution
//! (`구 민법 2007. 4. 11. 개정 전의 것`), ordinance/rule references.
//! Those can be layered on later; first priority is deterministic, auditable
//! links because hallucinated citations would be catastrophic in legal data.

use regex::Regex;
use std::sync::OnceLock;

/// A statute reference pulled from free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatuteRef {
    pub law_name: String,
    pub article: u32,
    pub article_sub: Option<u32>,
    /// The paragraph (`제N항`), captured for evidence context but not part of the node key.
    pub paragraph: Option<u32>,
    /// The sub-paragraph (`제N호`), captured for evidence.
    pub item: Option<u32>,
    /// Evidence — the raw matched substring.
    pub raw: String,
}

/// A Korean case number pulled from free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseRef {
    pub case_number: String,
    pub court: Option<String>,
    pub raw: String,
}

// ────────── Statute patterns ──────────

/// Matches `「법령명」` (bracketed law name) — most reliable signal.
fn bracketed_law_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"「\s*([^」\s][^」]*?)\s*」").unwrap())
}

/// Matches a bare law name followed by `제N조`. Used after a bracketed-law
/// pass to catch unbracketed references like `근로기준법 제36조`.
///
/// The law-name side accepts:
///   - a run of Korean characters ending in `법`/`령`/`률`/`규칙`/`조례`
///     (official names — `근로기준법`, `행정절차법`, `국토계획법`, …), OR
///   - a single-character short form in `['민','형','상']` followed by
///     whitespace + `제N조` (covers the bare-character conventions
///     `민 제750조` / `형 제250조`), OR
///   - a registered short form anywhere in `law_aliases::LAW_ALIAS_TABLE`
///     (e.g. `근기법`, `민소법`, `民法`, `刑法`) — these are checked
///     post-match via `canonical_name` so a short form resolves to the
///     same canonical law as its long form.
fn unbracketed_law_article_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            // 1st alternative: official long-form names ending in 법|령|률|규칙|조례
            // 2nd alternative: hanja forms (民法, 刑法) and Korean abbreviations ending in 법
            //                  — capped at 8 chars to avoid over-matching arbitrary text.
            r"((?:[가-힣][가-힣\s]{1,18}?(?:법|령|률|규칙|조례))|(?:[가-힣\p{Han}]{1,6}법))\s*제\s*(\d+)\s*조(?:의\s*(\d+))?(?:\s*제\s*(\d+)\s*항)?(?:\s*제\s*(\d+)\s*호)?"
        ).unwrap()
    })
}

/// Matches `제N조[의M][제K항][제L호]` — a bare article reference. Used once
/// a law-name context has been established by a prior match.
fn article_ref_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"제\s*(\d+)\s*조(?:의\s*(\d+))?(?:\s*제\s*(\d+)\s*항)?(?:\s*제\s*(\d+)\s*호)?",
        )
        .unwrap()
    })
}

/// Matches case numbers of the form `YYYY{유형}NNNNN`, e.g. `2024노3424`,
/// `2012도3166`, `2023가단92476`. The 유형 character set covers civil
/// (가/나/다/라/마), criminal (고/도/노/오), constitutional (헌/헌가/헌나/헌바),
/// administrative (구/두), and a few compound suffixes (`가단`/`고정`/`나단`).
fn case_number_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // (19|20)YY + 유형(1~3 Korean chars) + number
        Regex::new(r"((?:19|20)\d{2})([가-힣]{1,3})(\d{2,6})").unwrap()
    })
}

/// Court-name prefix regex (optional; used to tag `CaseRef.court`).
fn court_prefix_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(대법원|헌법재판소|서울고등법원|서울중앙지방법원|서울행정법원|서울가정법원|[가-힣]{2,6}(?:고등법원|지방법원|가정법원|행정법원)(?:\s*[가-힣]{2,4}지원)?)",
        )
        .unwrap()
    })
}

// ────────── Public API ──────────

/// Extract statute references from free text, carrying "same-law" context.
///
/// Strategy:
///   1. Split the text into segments at each bracketed-law marker (「…」) or
///      each unbracketed `{law}\s*제N조` match — these SET the law context.
///   2. Within each segment, greedy-match bare `제N조` patterns inheriting
///      the last seen law name.
///
/// Idempotent: the same citation (by law + article + sub) is not deduplicated
/// here — the caller can `HashSet` the output if desired. Raw text of each
/// match is preserved for evidence.
pub fn extract_statute_citations(text: &str, initial_law: Option<&str>) -> Vec<StatuteRef> {
    let mut out = Vec::new();
    let mut current_law: Option<String> = initial_law.map(str::to_string);

    // Pass 1: find all (position, law_name, article_info, consumed_range)
    // anchors where a law name is SET. Anchors come from bracketed markers
    // OR unbracketed law+article pairs. We merge these into a sorted list of
    // "law-context change" points.
    let mut anchors: Vec<(usize, String)> = Vec::new();
    for m in bracketed_law_re().captures_iter(text) {
        let whole = m.get(0).unwrap();
        let raw = m.get(1).unwrap().as_str().trim();
        // Canonicalise so `「근기법」 제43조` and `「근로기준법」 제43조`
        // produce the same citation (and thus the same slug).
        let law = super::law_aliases::canonical_name(raw);
        anchors.push((whole.end(), law));
    }
    for m in unbracketed_law_article_re().captures_iter(text) {
        let whole = m.get(0).unwrap();
        let raw = m.get(1).unwrap().as_str().trim();
        let law = super::law_aliases::canonical_name(raw);
        // Anchor is set at the START of the match so the article inside
        // the match itself uses this law.
        anchors.push((whole.start(), law));
    }
    anchors.sort_by_key(|&(pos, _)| pos);

    // Pass 2: for every bare article match, look up the most recent anchor
    // whose position ≤ match.start(); use that law. Fall back to `current_law`.
    for m in article_ref_re().captures_iter(text) {
        let whole = m.get(0).unwrap();
        let start = whole.start();

        // Find the applicable law context.
        let law = anchors
            .iter()
            .rev()
            .find(|(pos, _)| *pos <= start + 1) // +1 so anchor at same start counts
            .map(|(_, l)| l.clone())
            .or_else(|| current_law.clone());

        let Some(law) = law else {
            continue; // no law context known — skip rather than guess
        };

        let article: u32 = match m.get(1).and_then(|x| x.as_str().parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let article_sub: Option<u32> = m.get(2).and_then(|x| x.as_str().parse().ok());
        let paragraph: Option<u32> = m.get(3).and_then(|x| x.as_str().parse().ok());
        let item: Option<u32> = m.get(4).and_then(|x| x.as_str().parse().ok());

        // Keep current_law current for callers that pass the same text in
        // multiple chunks (not needed in this single-call flow, but cheap).
        current_law = Some(law.clone());

        out.push(StatuteRef {
            law_name: law,
            article,
            article_sub,
            paragraph,
            item,
            raw: whole.as_str().to_string(),
        });
    }

    out
}

/// Extract case numbers from text. Associates with a court name if the
/// nearest preceding court-prefix is within 30 characters.
pub fn extract_case_numbers(text: &str) -> Vec<CaseRef> {
    let mut out = Vec::new();
    // Precompute court matches with their end positions.
    let courts: Vec<(usize, String)> = court_prefix_re()
        .captures_iter(text)
        .filter_map(|c| {
            let m = c.get(0)?;
            Some((m.end(), m.as_str().trim().to_string()))
        })
        .collect();

    for m in case_number_re().captures_iter(text) {
        let whole = m.get(0).unwrap();
        let year = m.get(1).unwrap().as_str();
        let kind = m.get(2).unwrap().as_str();
        let num = m.get(3).unwrap().as_str();
        let case_number = format!("{year}{kind}{num}");

        let court = courts
            .iter()
            .rev()
            .find(|(end, _)| *end <= whole.start() && whole.start() - end <= 30)
            .map(|(_, c)| c.clone());

        out.push(CaseRef {
            case_number,
            court,
            raw: whole.as_str().to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refjo_block_parses_multi_law_with_inherited_context() {
        // Verbatim 참조조문 from the user's sample.
        let block = "형사소송법 제327조 제6호, 제364조 제6항, 제366조, 근로기준법 제36조, 제109조, 근로자퇴직급여 보장법 제9조 제1항, 제44조";
        let refs = extract_statute_citations(block, None);
        let triples: Vec<(String, u32, Option<u32>)> = refs
            .iter()
            .map(|r| (r.law_name.clone(), r.article, r.article_sub))
            .collect();
        assert_eq!(
            triples,
            vec![
                ("형사소송법".to_string(), 327, None),
                ("형사소송법".to_string(), 364, None),
                ("형사소송법".to_string(), 366, None),
                ("근로기준법".to_string(), 36, None),
                ("근로기준법".to_string(), 109, None),
                ("근로자퇴직급여 보장법".to_string(), 9, None),
                ("근로자퇴직급여 보장법".to_string(), 44, None),
            ]
        );
        // paragraph/item captured for the first one
        assert_eq!(refs[0].item, Some(6));
        assert_eq!(refs[1].paragraph, Some(6));
    }

    #[test]
    fn bracketed_law_is_honored() {
        let t = "「민법」 제767조에 따른 친족 중 대통령령으로 정하는 사람";
        let r = extract_statute_citations(t, None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].law_name, "민법");
        assert_eq!(r[0].article, 767);
    }

    #[test]
    fn article_with_sub_number() {
        let t = "근로기준법 제43조의2 제1항";
        let r = extract_statute_citations(t, None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].article, 43);
        assert_eq!(r[0].article_sub, Some(2));
        assert_eq!(r[0].paragraph, Some(1));
    }

    #[test]
    fn initial_law_fills_bare_refs() {
        // Inside statute body, bare `제36조` refers to the current law.
        let t = "제36조에 따른 임금 지급 의무를 위반한 자";
        let r = extract_statute_citations(t, Some("근로기준법"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].law_name, "근로기준법");
        assert_eq!(r[0].article, 36);
    }

    #[test]
    fn case_numbers_typical() {
        let t = "대법원 2012. 9. 13. 선고 2012도3166 판결 참조. 수원지방법원 2023가단92476";
        let r = extract_case_numbers(t);
        let nums: Vec<_> = r.iter().map(|c| c.case_number.as_str()).collect();
        assert!(nums.contains(&"2012도3166"));
        assert!(nums.contains(&"2023가단92476"));
    }

    #[test]
    fn case_numbers_compound_kind() {
        // 고정 is two Hangul chars — should match.
        let t = "원심 2024고정48 판결";
        let r = extract_case_numbers(t);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].case_number, "2024고정48");
    }

    #[test]
    fn empty_when_no_law_context() {
        // No law context, no bracketed, no law+article anchor → skip.
        let t = "제43조에 따른 기준";
        let r = extract_statute_citations(t, None);
        assert!(r.is_empty());
    }

    #[test]
    fn short_form_citations_canonicalise_to_official_law() {
        // `근기법 제36조` should resolve to 근로기준법.
        let t = "근기법 제36조, 제109조";
        let r = extract_statute_citations(t, None);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|c| c.law_name == "근로기준법"));
    }

    #[test]
    fn mixed_short_and_long_forms_in_one_block() {
        // A refjo-like block mixing 근기법 + 근퇴법 + 형소법.
        let t = "근기법 제36조, 근퇴법 제9조 제1항, 형소법 제327조";
        let r = extract_statute_citations(t, None);
        let pairs: Vec<(&str, u32)> = r
            .iter()
            .map(|c| (c.law_name.as_str(), c.article))
            .collect();
        assert!(pairs.contains(&("근로기준법", 36)));
        assert!(pairs.contains(&("근로자퇴직급여 보장법", 9)));
        assert!(pairs.contains(&("형사소송법", 327)));
    }

    #[test]
    fn bracketed_short_form_also_canonicalises() {
        let t = "「근기법」 제43조의2";
        let r = extract_statute_citations(t, None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].law_name, "근로기준법");
        assert_eq!(r[0].article, 43);
        assert_eq!(r[0].article_sub, Some(2));
    }

    #[test]
    fn unknown_law_name_passes_through_unchanged() {
        // An unrecognised name shouldn't be aliased away — we preserve
        // whatever the user wrote so we don't silently re-anchor to a
        // different law.
        let t = "지어낸법 제5조";
        let r = extract_statute_citations(t, None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].law_name, "지어낸법");
    }
}
