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
///   - a hanja form or Korean abbreviation ending in `법` (`民法`,
///     `刑法`, `근기법`, `민소법`, …) — capped at 6 chars to avoid
///     over-matching arbitrary text. Short forms canonicalise
///     post-match via `law_aliases::canonical_name`.
///
/// Optional `구\s+` prefix captures Korean legal "former law" references
/// (`구 민법 제839조의2`). The prefix is outside the law-name capture
/// group so the matched name stays clean; `canonical_name` would strip
/// any residual prefix anyway via `strip_revision_prefix`.
///
/// Optional parenthetical between law name and `제N조` carries the
/// revision-date context that typically follows `구 {법}` references —
/// e.g. `구 민법 (2007. 12. 21. 법률 제8720호로 개정되기 전의 것)
/// 제839조의2`. The parenthetical is non-capturing; the whole match's
/// `raw` field preserves it as edge evidence.
fn unbracketed_law_article_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            // (?:구법?\s+)?                                     optional 구/구법 prefix
            // ( official | abbreviation ) capture              law name
            // (?:\s*\([^)]{1,200}\))?                           optional parenthetical (revision date, etc.)
            // \s*제\s*(\d+)\s*조(?:의\s*(\d+))?                 article + sub
            // (?:\s*제\s*(\d+)\s*항)?(?:\s*제\s*(\d+)\s*호)?    paragraph + item
            r"(?:구법?\s+)?((?:[가-힣][가-힣\s]{1,18}?(?:법|령|률|규칙|조례))|(?:[가-힣\p{Han}]{1,6}법))(?:\s*\([^)]{1,200}\))?\s*제\s*(\d+)\s*조(?:의\s*(\d+))?(?:\s*제\s*(\d+)\s*항)?(?:\s*제\s*(\d+)\s*호)?"
        ).unwrap()
    })
}

/// Matches `제N1조[의M1] {범위 구분자} 제N2조[의M2]` — a closed article
/// range. Used to expand citations like `제36조 내지 제40조` into
/// individual references for every integer in the range.
///
/// Supports two range separators seen in Korean legal writing:
///   - `내지` (the formal legal term, by far the most common)
///   - `부터 ... 까지` (less common; captured but the `까지` tail is
///     consumed as context — the underlying regex simplification is
///     that `(?:내지|부터)` produces a clean "from-to" bracket)
fn article_range_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"제\s*(\d+)\s*조(?:의\s*(\d+))?\s*(?:내지|부터)\s*제\s*(\d+)\s*조(?:의\s*(\d+))?",
        )
        .unwrap()
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
    // Anchor = (pick_position, span_start, law). `pick_position` is used for
    // "most recent anchor before this article" lookup; `span_start` is the
    // actual text index where the anchor's region BEGINS, so `raw` evidence
    // captures the law name and any parenthetical (e.g. a revision-date
    // block) in addition to the article reference.
    let mut anchors: Vec<(usize, usize, String)> = Vec::new();
    for m in bracketed_law_re().captures_iter(text) {
        let whole = m.get(0).unwrap();
        let raw = m.get(1).unwrap().as_str().trim();
        // Canonicalise so `「근기법」 제43조` and `「근로기준법」 제43조`
        // produce the same citation (and thus the same slug).
        let law = super::law_aliases::canonical_name(raw);
        anchors.push((whole.end(), whole.start(), law));
    }
    for m in unbracketed_law_article_re().captures_iter(text) {
        let whole = m.get(0).unwrap();
        let raw = m.get(1).unwrap().as_str().trim();
        let law = super::law_aliases::canonical_name(raw);
        // For pick_position use whole.start() so the article inside the
        // match itself uses this law (matches the existing test expecting
        // `pos ≤ article_start + 1`).
        anchors.push((whole.start(), whole.start(), law));
    }
    anchors.sort_by_key(|&(pick, _, _)| pick);

    // Pass 1.5: expand `제N1조 내지 제N2조` ranges BEFORE the bare-article
    // pass consumes the endpoints individually. For each range we emit one
    // StatuteRef per integer in [N1, N2] (main article numbers only —
    // sub-articles like `제36조의2` inside the range would require per-law
    // knowledge to enumerate correctly, so we emit just the start/end
    // sub-articles if present). Hard cap of `MAX_RANGE_EXPANSION`
    // protects against pathological `제1조 내지 제1000조`.
    const MAX_RANGE_EXPANSION: u32 = 50;
    let mut ranges_consumed: Vec<(usize, usize)> = Vec::new();
    for m in article_range_re().captures_iter(text) {
        let whole = m.get(0).unwrap();
        let start = whole.start();
        let start_art: u32 = match m.get(1).and_then(|x| x.as_str().parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let start_sub: Option<u32> = m.get(2).and_then(|x| x.as_str().parse().ok());
        let end_art: u32 = match m.get(3).and_then(|x| x.as_str().parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let end_sub: Option<u32> = m.get(4).and_then(|x| x.as_str().parse().ok());
        if end_art < start_art {
            continue; // malformed range; leave for Pass 2 to handle endpoints individually
        }
        // Find applicable law.
        let anchor = anchors.iter().rev().find(|(pick, _, _)| *pick <= start + 1);
        let (law, raw_start) = match anchor {
            Some((_, s, l)) => (l.clone(), *s),
            None => match current_law.clone() {
                Some(l) => (l, start),
                None => continue,
            },
        };
        let raw_slice = if raw_start < whole.end()
            && text.is_char_boundary(raw_start)
            && text.is_char_boundary(whole.end())
        {
            text[raw_start..whole.end()].to_string()
        } else {
            whole.as_str().to_string()
        };
        // Emit the endpoints (with their sub-articles if any).
        out.push(StatuteRef {
            law_name: law.clone(),
            article: start_art,
            article_sub: start_sub,
            paragraph: None,
            item: None,
            raw: raw_slice.clone(),
        });
        // Middle articles — cap at MAX_RANGE_EXPANSION to avoid blow-up.
        let span_len = end_art.saturating_sub(start_art);
        let capped_end = if span_len > MAX_RANGE_EXPANSION {
            start_art + MAX_RANGE_EXPANSION
        } else {
            end_art
        };
        for n in (start_art + 1)..capped_end {
            out.push(StatuteRef {
                law_name: law.clone(),
                article: n,
                article_sub: None,
                paragraph: None,
                item: None,
                raw: raw_slice.clone(),
            });
        }
        // Final endpoint (use end_sub if same as capped; otherwise the raw regex end).
        if capped_end == end_art {
            out.push(StatuteRef {
                law_name: law.clone(),
                article: end_art,
                article_sub: end_sub,
                paragraph: None,
                item: None,
                raw: raw_slice.clone(),
            });
        }
        current_law = Some(law);
        ranges_consumed.push((whole.start(), whole.end()));
    }

    // Pass 2: for every bare article match, look up the most recent anchor
    // whose position ≤ match.start(); use that law. Fall back to
    // `current_law`. The anchor's `span_start` becomes the start of the
    // `raw` evidence slice so we preserve "구 민법 (2007. 12. 21. …로
    // 개정되기 전의 것)" around `제839조의2 제1항`.
    for m in article_ref_re().captures_iter(text) {
        let whole = m.get(0).unwrap();
        let start = whole.start();

        // Skip matches that fall inside a range already expanded by
        // Pass 1.5 — otherwise the endpoints of `제36조 내지 제40조`
        // would produce duplicate StatuteRefs.
        if ranges_consumed
            .iter()
            .any(|(s, e)| *s <= start && whole.end() <= *e)
        {
            continue;
        }

        // Find the applicable law context.
        let anchor_match = anchors
            .iter()
            .rev()
            .find(|(pick, _, _)| *pick <= start + 1); // +1 so anchor at same start counts
        let (law, raw_start) = match anchor_match {
            Some((_, span_start, l)) => (l.clone(), *span_start),
            None => match current_law.clone() {
                Some(l) => (l, start),
                None => continue, // no law context known — skip rather than guess
            },
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

        // Build evidence spanning from the anchor (if any) through the
        // article reference. Guard against negative / crossed ranges that
        // could arise if an anchor pick_position preceded its span_start
        // (shouldn't happen but be defensive with char-boundary safety).
        let end = whole.end();
        let raw_slice = if raw_start < end && text.is_char_boundary(raw_start) && text.is_char_boundary(end) {
            &text[raw_start..end]
        } else {
            whole.as_str()
        };

        out.push(StatuteRef {
            law_name: law,
            article,
            article_sub,
            paragraph,
            item,
            raw: raw_slice.to_string(),
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

    #[test]
    fn old_law_prefix_stripped_simple() {
        // `구 민법 제839조의2` resolves to the same law as `민법 제839조의2`.
        let t = "구 민법 제839조의2";
        let r = extract_statute_citations(t, None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].law_name, "민법");
        assert_eq!(r[0].article, 839);
        assert_eq!(r[0].article_sub, Some(2));
    }

    #[test]
    fn old_law_prefix_with_revision_date_parenthetical() {
        // Real-world form with the long revision-date block between the
        // law name and the article reference. The parenthetical is
        // preserved in the `raw` evidence field but does not prevent
        // the match.
        let t = "구 민법(2007. 12. 21. 법률 제8720호로 개정되기 전의 것) 제839조의2 제1항";
        let r = extract_statute_citations(t, None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].law_name, "민법");
        assert_eq!(r[0].article, 839);
        assert_eq!(r[0].article_sub, Some(2));
        assert_eq!(r[0].paragraph, Some(1));
        // Evidence preserves the revision date so the edge's `context`
        // carries full audit information.
        assert!(r[0].raw.contains("2007. 12. 21."));
        assert!(r[0].raw.contains("개정되기 전의 것"));
    }

    #[test]
    fn old_law_prefix_with_spaced_parenthetical() {
        // Same pattern, but with a space between the law name and `(`.
        let t = "구 근로기준법 (2024. 10. 22. 법률 제20520호로 개정되기 전의 것) 제36조";
        let r = extract_statute_citations(t, None);
        assert!(r.iter().any(|c| c.law_name == "근로기준법" && c.article == 36));
    }

    #[test]
    fn old_law_mixed_with_current_in_same_block() {
        // Mixed refjo block: some references are current, some are 구법.
        // All `민법` references (current or 구) must land on the same slug;
        // all `근로기준법` references likewise. Edge evidence will tell
        // the caller which was a current-version vs. historical citation.
        let t = "민법 제750조, 구 민법(2007. 12. 21. 법률 제8720호로 개정되기 전의 것) 제839조의2, 근로기준법 제36조";
        let r = extract_statute_citations(t, None);
        let pairs: Vec<(&str, u32, Option<u32>)> = r
            .iter()
            .map(|c| (c.law_name.as_str(), c.article, c.article_sub))
            .collect();
        assert!(pairs.contains(&("민법", 750, None)));
        assert!(pairs.contains(&("민법", 839, Some(2))));
        assert!(pairs.contains(&("근로기준법", 36, None)));
    }

    #[test]
    fn bracketed_old_law_also_canonicalises() {
        // `「구 민법」 제839조의2` — 구 prefix inside the bracket.
        // canonical_name strips the prefix so the slug is `statute::민법::839-2`.
        let t = "「구 민법」 제839조의2";
        let r = extract_statute_citations(t, None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].law_name, "민법");
        assert_eq!(r[0].article, 839);
        assert_eq!(r[0].article_sub, Some(2));
    }

    #[test]
    fn range_naeji_expands_to_every_article_in_between() {
        // `제36조 내지 제40조` → articles 36, 37, 38, 39, 40.
        let t = "근로기준법 제36조 내지 제40조";
        let r = extract_statute_citations(t, None);
        let nums: Vec<u32> = r.iter().map(|c| c.article).collect();
        assert_eq!(nums, vec![36, 37, 38, 39, 40]);
        assert!(r.iter().all(|c| c.law_name == "근로기준법"));
        // Range endpoints Pass 2 would have matched must not be
        // duplicated — the ranges_consumed guard suppresses them.
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn range_honors_sub_articles_at_endpoints() {
        let t = "근로기준법 제43조의2 내지 제43조의5";
        let r = extract_statute_citations(t, None);
        // Start and end get their sub-articles; middle fillers use the
        // main article number only (can't infer sub-articles without
        // per-law knowledge).
        assert_eq!(r.first().unwrap().article_sub, Some(2));
        assert_eq!(r.last().unwrap().article_sub, Some(5));
        // Middle articles share the same main number 43 without subs.
        assert!(r[1..r.len() - 1]
            .iter()
            .all(|c| c.article == 43 && c.article_sub.is_none()));
    }

    #[test]
    fn range_caps_huge_expansions_at_50() {
        let t = "민법 제1조 내지 제1000조";
        let r = extract_statute_citations(t, None);
        // Cap of 50 articles; endpoint included separately only if within cap.
        assert!(r.len() <= 51, "expected ≤51 refs, got {}", r.len());
        assert_eq!(r[0].article, 1);
    }

    #[test]
    fn middle_dot_list_matches_each_article_independently() {
        // Middle-dot separators (`·` U+00B7 and `ㆍ` U+318D) must not
        // break the law-name anchor inheritance for the subsequent
        // articles. Tests both variants.
        for sep in &["·", "ㆍ", ", "] {
            let t = format!("근로기준법 제36조{s}제43조{s}제56조", s = sep);
            let r = extract_statute_citations(&t, None);
            let nums: Vec<u32> = r.iter().map(|c| c.article).collect();
            assert_eq!(
                nums,
                vec![36, 43, 56],
                "separator `{sep}` failed: {:?}",
                r
            );
            assert!(r.iter().all(|c| c.law_name == "근로기준법"));
        }
    }

    #[test]
    fn malformed_range_falls_through_to_individual_endpoints() {
        // Reverse-order range — we treat it as malformed and let Pass 2
        // pick up the endpoints as independent citations.
        let t = "근로기준법 제40조 내지 제36조";
        let r = extract_statute_citations(t, None);
        let nums: Vec<u32> = r.iter().map(|c| c.article).collect();
        // Two individual matches (40 and 36), no range expansion.
        assert_eq!(nums, vec![40, 36]);
    }
}
