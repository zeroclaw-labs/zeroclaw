//! Shared Korean-legal date parsing helpers.
//!
//! Used by:
//!   - `statute_extractor` supplement body → 시행일 (effective date)
//!   - `statute_extractor` article body → `<개정 …>` / `<신설 …>` tags
//!   - `case_extractor` 판결이유 body → 사건발생일 (incident dates)
//!
//! Normalised output is **YYYYMMDD** (8-digit string). Invalid month/day
//! combinations are rejected so downstream range queries stay sound.

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use std::sync::OnceLock;

/// Matches Korean date-literal forms commonly found in legal text:
///   - `2025. 10. 1.` / `2025.10.1` / `2025. 10. 01`
///   - `2025년 10월 1일` / `2025년 10월 01일`
///   - `2025-10-01`
/// Does NOT accept partial-date forms like `2025. 10.` (month only).
fn literal_date_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(\d{4})\s*[년.\-]\s*(\d{1,2})\s*[월.\-]\s*(\d{1,2})\s*[일.]?",
        )
        .unwrap()
    })
}

/// Matches supplements' `이 법은 공포(한 날|일)부터 시행한다` phrasing.
fn promulgation_day_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"공포한?\s*날\s*(?:부터|로부터|에)\s*시행").unwrap()
    })
}

/// Matches relative-offset forms like:
///   - `공포 후 6개월이 경과한 날부터 시행`
///   - `공포 후 3개월 후부터 시행`
///   - `공포 후 60일이 경과한 날부터`
/// Captures the numeric amount and the unit (`개월` or `일`).
fn relative_offset_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"공포\s*(?:한\s*날로?부터|후)\s*(\d+)\s*(개월|일)").unwrap()
    })
}

/// Find every YYYY-MM-DD literal in `text`, normalised to YYYYMMDD and
/// chronologically sorted. Ignores malformed dates (bad month/day).
/// De-duplicates while preserving sort order.
pub fn find_all_dates(text: &str) -> Vec<String> {
    let mut out: Vec<NaiveDate> = literal_date_re()
        .captures_iter(text)
        .filter_map(|c| {
            let y: i32 = c.get(1)?.as_str().parse().ok()?;
            let m: u32 = c.get(2)?.as_str().parse().ok()?;
            let d: u32 = c.get(3)?.as_str().parse().ok()?;
            NaiveDate::from_ymd_opt(y, m, d)
        })
        .collect();
    out.sort();
    out.dedup();
    out.into_iter().map(fmt_yyyymmdd).collect()
}

/// Parse a single supplement body fragment for its effective date.
///
/// Priority:
///   1. Explicit literal date inside a `...부터 시행` clause
///      (`이 법은 2025년 10월 1일부터 시행한다`)
///   2. `공포한 날` phrasing → returns `promulgation_date` if provided
///   3. Relative offset (`공포 후 N개월 … 시행`) → promulgation + N
///   4. Nothing recognised → `None`
///
/// Per Korean legal convention, if a supplement omits an effective date
/// entirely, the statute takes effect on the promulgation date. We
/// surface that as `Some(promulgation_date)` when the body mentions
/// `공포한 날` OR the body is empty/unrecognised AND a promulgation
/// date is known — but we leave the "default to promulgation" decision
/// to the caller by only applying rules 2 and 3 here.
pub fn parse_supplement_effective_date(
    body: &str,
    promulgation_date: Option<&str>,
) -> Option<String> {
    // Narrow to the 시행일 clause when possible — the supplement body
    // often contains dated commencement exceptions (`다만, 제XX조는 …`)
    // that shouldn't outrank the primary date.
    let scope = find_primary_commencement_scope(body).unwrap_or(body);

    // Rule 1: explicit date adjacent to "시행".
    if let Some(d) = literal_date_near_sihaeng(scope) {
        return Some(d);
    }

    // Rule 2: "공포한 날" → promulgation date.
    if promulgation_day_re().is_match(scope) {
        return promulgation_date.map(str::to_string);
    }

    // Rule 3: relative offset from promulgation.
    if let Some(caps) = relative_offset_re().captures(scope) {
        let amount: i64 = caps.get(1)?.as_str().parse().ok()?;
        let unit = caps.get(2)?.as_str();
        let base = promulgation_date.and_then(parse_yyyymmdd)?;
        let offset = match unit {
            "개월" => add_months(base, amount)?,
            "일" => base.checked_add_signed(chrono::Duration::days(amount))?,
            _ => return None,
        };
        return Some(fmt_yyyymmdd(offset));
    }

    None
}

/// Extract every `<개정 …>` / `<신설 …>` / `[시행일 …]` tag from an article
/// body, returning the dates in chronological order (dedup). Non-date
/// content inside the tags is ignored.
pub fn extract_article_amendment_dates(body: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"[<\[](?:개정|신설|전문개정|시행일)\s*([^>\]]+)[>\]]").unwrap()
    });
    let mut dates: Vec<NaiveDate> = re
        .captures_iter(body)
        .flat_map(|c| {
            let inside = c.get(1).map(|m| m.as_str()).unwrap_or("");
            literal_date_re()
                .captures_iter(inside)
                .filter_map(|d| {
                    let y: i32 = d.get(1)?.as_str().parse().ok()?;
                    let mo: u32 = d.get(2)?.as_str().parse().ok()?;
                    let da: u32 = d.get(3)?.as_str().parse().ok()?;
                    NaiveDate::from_ymd_opt(y, mo, da)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    dates.sort();
    dates.dedup();
    dates.into_iter().map(fmt_yyyymmdd).collect()
}

/// Fallback signal when 판례 body has no explicit date literal. Each
/// variant carries the year the caller should base the incident-date
/// range on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidentDateFallback {
    /// Year extracted from the `【원심판결】 … YYYY유형NNNN …` line.
    /// Caller should widen to `[Y-2, Y-1]` to cover both 항소심
    /// (원심 = 1심, one level below) and 대법원 (원심 = 항소심, two
    /// levels below) — each court tier takes roughly one year.
    WonsimYear(u16),
    /// Filing year of the judgment's own case number. Used when the
    /// body has no `【원심판결】` marker, typical of 1st-instance
    /// judgments where the 소장 접수년도 ≈ 사건발생년도.
    OwnCaseYear(u16),
    /// No usable signal.
    None,
}

/// Infer a year-granularity incident-date signal when the case body has
/// no explicit date literal. Applies the domain rule the user specified:
///
///   * **참고판례** / **관련사건** references across the body are
///     ignored — they're OTHER cases unrelated to this judgment's
///     incident date.
///   * **`【원심판결】`** is the ONLY trustworthy signal for 2심 /
///     상고심 judgments. The case number on that line encodes the
///     year of the preceding court's filing; incidents happen
///     1-2 years earlier (a 1심 takes ~1 year, an 항소심 another ~1).
///   * 1심 judgments (no 원심판결) fall back to their own case year,
///     which approximates the filing = incident year.
///
/// The returned year is sanity-checked to `1950 ≤ y ≤ current+1` so
/// 4-digit captures from ID strings can't poison the result.
pub fn infer_incident_date_fallback(
    body: &str,
    own_case_number: &str,
) -> IncidentDateFallback {
    let cur_year = chrono::Utc::now().year() as u16 + 1;
    let ok = |y: u16| -> bool { (1950..=cur_year).contains(&y) };

    if let Some(y) = extract_wonsim_case_year(body).filter(|&y| ok(y)) {
        return IncidentDateFallback::WonsimYear(y);
    }
    let own_year: Option<u16> = own_case_number
        .chars()
        .take(4)
        .collect::<String>()
        .parse()
        .ok()
        .filter(|&y| ok(y));
    match own_year {
        Some(y) => IncidentDateFallback::OwnCaseYear(y),
        None => IncidentDateFallback::None,
    }
}

/// Extract the case-number year from the `【원심판결】` line, or `None`
/// if the marker is absent. We scope to the single logical line (up to
/// 300 bytes or the next newline) so unrelated later references
/// — notably 참고판례 (`참조판례`) blocks — can't leak into the match.
fn extract_wonsim_case_year(body: &str) -> Option<u16> {
    // Korean case files use both 【원심판결】 and 【원심】 in practice.
    let pos = body
        .find("【원심판결】")
        .or_else(|| body.find("【원심】"))
        .or_else(|| body.find("원심판결】"))?;
    let rest = &body[pos..];
    let end = rest
        .find('\n')
        .unwrap_or(rest.len())
        .min(300);
    let end = std::cmp::min(end, rest.len());
    // Snap to char boundary so we don't slice mid-UTF-8.
    let scope = if rest.is_char_boundary(end) {
        &rest[..end]
    } else {
        // Walk back to the nearest char boundary.
        let mut e = end;
        while e > 0 && !rest.is_char_boundary(e) {
            e -= 1;
        }
        &rest[..e]
    };

    let cases = super::citation_patterns::extract_case_numbers(scope);
    cases.first().and_then(|c| {
        c.case_number
            .chars()
            .take(4)
            .collect::<String>()
            .parse::<u16>()
            .ok()
    })
}

// ───────── Internals ─────────

/// Isolate the `제1조(시행일)` clause or its closest analogue so later
/// "다만, 제XX조의 개정규정은 …부터 시행한다" exceptions don't override
/// the primary date.
fn find_primary_commencement_scope(body: &str) -> Option<&str> {
    // Look for `제1조(시행일)` ... `제2조` or end-of-text.
    static HEADER: OnceLock<Regex> = OnceLock::new();
    let hdr = HEADER.get_or_init(|| Regex::new(r"제\s*1\s*조\s*\(\s*시행일\s*\)").unwrap());
    let m = hdr.find(body)?;
    let rest = &body[m.end()..];
    // Cut at next `제N조` header so subsequent articles don't leak in.
    static NEXT: OnceLock<Regex> = OnceLock::new();
    let nx = NEXT.get_or_init(|| Regex::new(r"제\s*\d+\s*조").unwrap());
    match nx.find(rest) {
        Some(boundary) => Some(&rest[..boundary.start()]),
        None => Some(rest),
    }
}

/// Find the first literal date that sits within ~80 chars of a `시행`
/// occurrence. Small window so we don't pick up amendment-history
/// dates elsewhere in the clause.
fn literal_date_near_sihaeng(scope: &str) -> Option<String> {
    // Iterate over "시행" occurrences and check a window around each.
    let mut best: Option<NaiveDate> = None;
    let sihaeng_positions: Vec<usize> = scope.match_indices("시행").map(|(i, _)| i).collect();
    if sihaeng_positions.is_empty() {
        return None;
    }
    for m in literal_date_re().captures_iter(scope) {
        let whole = m.get(0)?;
        let dpos = whole.start();
        // Must be within 80 chars (before or after) of some "시행".
        let close = sihaeng_positions
            .iter()
            .any(|&sp| (sp.max(dpos) - sp.min(dpos)) <= 80);
        if !close {
            continue;
        }
        let y: i32 = m.get(1)?.as_str().parse().ok()?;
        let mo: u32 = m.get(2)?.as_str().parse().ok()?;
        let da: u32 = m.get(3)?.as_str().parse().ok()?;
        let parsed = NaiveDate::from_ymd_opt(y, mo, da)?;
        if best.map_or(true, |b| parsed < b) {
            best = Some(parsed);
        }
    }
    best.map(fmt_yyyymmdd)
}

fn fmt_yyyymmdd(d: NaiveDate) -> String {
    format!("{:04}{:02}{:02}", d.year(), d.month(), d.day())
}

fn parse_yyyymmdd(s: &str) -> Option<NaiveDate> {
    if s.len() != 8 {
        return None;
    }
    let y: i32 = s[..4].parse().ok()?;
    let m: u32 = s[4..6].parse().ok()?;
    let d: u32 = s[6..8].parse().ok()?;
    NaiveDate::from_ymd_opt(y, m, d)
}

fn add_months(base: NaiveDate, months: i64) -> Option<NaiveDate> {
    let total = base.year() as i64 * 12 + base.month0() as i64 + months;
    let y = total.div_euclid(12) as i32;
    let m = (total.rem_euclid(12) + 1) as u32;
    let d = base.day();
    // Clamp day to month length (handles Feb 30 etc.).
    let last = days_in_month(y, m);
    NaiveDate::from_ymd_opt(y, m, d.min(last))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let first_next = NaiveDate::from_ymd_opt(ny, nm, 1).unwrap();
    let first_this = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
    (first_next - first_this).num_days() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_all_dates_normalises_forms() {
        let t = "계약은 2024. 3. 28. 체결되었고, 2024년 5월 1일 해지되었다.";
        assert_eq!(
            find_all_dates(t),
            vec!["20240328".to_string(), "20240501".to_string()]
        );
    }

    #[test]
    fn find_all_dates_filters_invalid() {
        let t = "2024. 13. 45."; // bad month/day
        assert!(find_all_dates(t).is_empty());
    }

    #[test]
    fn find_all_dates_dedups() {
        let t = "2024. 3. 28. ... 2024년 3월 28일";
        assert_eq!(find_all_dates(t), vec!["20240328"]);
    }

    #[test]
    fn supplement_explicit_date() {
        let body = "제1조(시행일) 이 법은 2025년 10월 1일부터 시행한다.";
        let d = parse_supplement_effective_date(body, Some("20251001"));
        assert_eq!(d.as_deref(), Some("20251001"));
    }

    #[test]
    fn supplement_promulgation_day_uses_fallback() {
        let body = "이 법은 공포한 날부터 시행한다.";
        let d = parse_supplement_effective_date(body, Some("20240411"));
        assert_eq!(d.as_deref(), Some("20240411"));
    }

    #[test]
    fn supplement_relative_offset_months() {
        let body = "이 법은 공포 후 6개월이 경과한 날부터 시행한다.";
        let d = parse_supplement_effective_date(body, Some("20250101"));
        assert_eq!(d.as_deref(), Some("20250701"));
    }

    #[test]
    fn supplement_relative_offset_days() {
        let body = "이 법은 공포 후 60일이 경과한 날부터 시행한다.";
        let d = parse_supplement_effective_date(body, Some("20250101"));
        assert_eq!(d.as_deref(), Some("20250302"));
    }

    #[test]
    fn supplement_primary_scope_ignores_exception_clause() {
        // Exception clause `다만, 제XX조는 …` must not outrank primary.
        let body = "제1조(시행일) 이 법은 2025년 10월 1일부터 시행한다. 다만, \
                    제8조의 개정규정은 2026년 1월 1일부터 시행한다.";
        let d = parse_supplement_effective_date(body, Some("20251001"));
        assert_eq!(d.as_deref(), Some("20251001"));
    }

    #[test]
    fn supplement_unrecognised_returns_none() {
        let body = "아무말 없음";
        assert!(parse_supplement_effective_date(body, Some("20250101")).is_none());
    }

    #[test]
    fn article_amendment_dates_extracts_comma_list() {
        let body =
            "제2조(정의) ① 이 법에서 사용하는 용어의 뜻은 다음과 같다. \
             <개정 2018. 3. 20., 2019. 1. 15., 2020. 5. 26.>";
        let dates = extract_article_amendment_dates(body);
        assert_eq!(dates, vec!["20180320", "20190115", "20200526"]);
    }

    #[test]
    fn article_amendment_dates_handles_sinseol_and_sihaengil_tags() {
        let body = "제8조 ① ... <신설 2021. 4. 13.> ... [시행일 2022. 1. 1.]";
        let dates = extract_article_amendment_dates(body);
        assert_eq!(dates, vec!["20210413", "20220101"]);
    }

    #[test]
    fn article_amendment_dates_ignores_dates_outside_tags() {
        // Only tagged dates should be captured, not free text.
        let body = "제1조. 이 조문은 2025년 1월 1일에 발효된다.";
        assert!(extract_article_amendment_dates(body).is_empty());
    }

    #[test]
    fn fallback_uses_wonsim_line_only_ignoring_references() {
        // 참고판례 / 관련사건 have OTHER incident dates; only 【원심판결】
        // carries a signal that tracks the current judgment's timeline.
        let body = "【원심판결】 수원지법 안산지원 선고 2024고정48 판결. \
                    참조판례: 대법원 2012. 9. 13. 선고 2012도3166 판결. \
                    관련 민사사건: 2023가단92476.";
        let fb = infer_incident_date_fallback(body, "2024노3424");
        // Must pick 2024 from the 원심판결 line, NOT 2012 from 참조판례.
        assert_eq!(fb, IncidentDateFallback::WonsimYear(2024));
    }

    #[test]
    fn fallback_falls_through_to_own_year_when_no_wonsim_marker() {
        // 1st-instance judgment — no 원심판결 marker at all. Own case
        // year is the best approximation.
        let body = "본건은 2024노3424 사건이다.";
        let fb = infer_incident_date_fallback(body, "2024노3424");
        assert_eq!(fb, IncidentDateFallback::OwnCaseYear(2024));
    }

    #[test]
    fn fallback_handles_short_wonsim_marker_variant() {
        // Some judgments abbreviate to 【원심】.
        let body = "【원심】 서울고등법원 2023나12345 판결";
        let fb = infer_incident_date_fallback(body, "2024다67890");
        assert_eq!(fb, IncidentDateFallback::WonsimYear(2023));
    }

    #[test]
    fn fallback_returns_none_only_when_own_case_year_is_malformed() {
        let fb = infer_incident_date_fallback("", "잘못된사건번호");
        assert_eq!(fb, IncidentDateFallback::None);
    }

    #[test]
    fn fallback_rejects_implausible_years() {
        // 9999 is future; rejected in both 원심 extraction and own-case.
        let body = "【원심판결】 9999다12345";
        let fb = infer_incident_date_fallback(body, "2024노3424");
        // 원심 year rejected → falls through to own case year.
        assert_eq!(fb, IncidentDateFallback::OwnCaseYear(2024));
    }
}
