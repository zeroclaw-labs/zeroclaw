//! Parse a Korean precedent (판례) markdown file into a [`CaseDoc`].
//!
//! Signal sources (in order of reliability):
//!   1. **Filename**  — `{선고일YYYYMMDD}/{사건번호}_{사건종류코드}_{사건종류}_{판례정보일련번호}_{판결유형}_{관할법원}_{사건명}.md`
//!      The parent directory name is the verdict date.
//!   2. **`## field` headers** — structured sections the user's pipeline
//!      writes (사건번호, 법원명, 선고일자, 사건종류코드, 판례정보일련번호,
//!      선고, 판결유형, 법원종류코드, 사건종류명, 사건명, 판시사항, 판결요지,
//!      참조조문, 참조판례, 판례내용).
//!
//! The **참조조문** section is the most valuable edge source — it's the user's
//! "most direct link between statute and case." We parse it with the
//! same-law-inheritance rules in `citation_patterns::extract_statute_citations`.
//! **참조판례** is the case↔case edge source.

use super::citation_patterns::{
    extract_case_numbers, extract_statute_citations, CaseRef, StatuteRef,
};
use super::slug::case_slug;
use anyhow::{anyhow, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// Parsed representation of a 판례 markdown file.
#[derive(Debug, Clone)]
pub struct CaseDoc {
    pub slug: String,
    /// 사건번호, e.g. `2024노3424`.
    pub case_number: String,
    pub case_name: Option<String>,
    pub court_name: Option<String>,
    pub court_type_code: Option<String>,
    pub case_category_code: Option<String>,
    pub case_type_name: Option<String>,
    pub precedent_serial_no: Option<String>,
    /// YYYYMMDD string.
    pub verdict_date: Option<String>,
    pub verdict_kind: Option<String>, // `선고`
    pub verdict_type: Option<String>, // `판결 : 환송` etc.
    pub holding: Option<String>,      // 판시사항
    pub summary: Option<String>,      // 판결요지
    pub body: Option<String>,         // 판례내용
    pub source_path: String,
    /// Citations to statute articles (from 참조조문 primarily, plus body).
    pub statute_citations: Vec<StatuteRef>,
    /// Citations to other cases (from 참조판례 primarily, plus body).
    pub case_citations: Vec<CaseRef>,
    /// Full original markdown, for storage in `vault_documents.content`.
    pub original_markdown: String,
}

pub fn looks_like_case(md: &str) -> bool {
    md.contains("## 사건번호") || md.contains("## 판결요지") || md.contains("## 참조조문")
}

pub fn extract_case(md: &str, source_path: &str) -> Result<CaseDoc> {
    let sections = split_into_sections(md);
    let path_meta = parse_path_metadata(source_path);

    // 사건번호 — prefer explicit `## 사건번호` header, fall back to filename.
    let case_number = sections
        .get("사건번호")
        .map(|s| s.trim().to_string())
        .or(path_meta.case_number.clone())
        .ok_or_else(|| {
            anyhow!("case markdown: no 사건번호 (header or filename) in {source_path}")
        })?;

    let case_name = sections
        .get("사건명")
        .map(|s| s.trim().to_string())
        .or(path_meta.case_name.clone());
    let court_name = sections
        .get("법원명")
        .map(|s| s.trim().to_string())
        .or(path_meta.court_name.clone());
    let verdict_date = sections
        .get("선고일자")
        .map(|s| s.trim().to_string())
        .or(path_meta.verdict_date.clone());

    // Primary edge extraction: 참조조문 → statute citations.
    let mut statute_cits = sections
        .get("참조조문")
        .map(|s| extract_statute_citations(s, None))
        .unwrap_or_default();
    // Secondary: body mentions (e.g. 대법원 2012. 9. 13. 선고 2012도3166 판결).
    if let Some(body) = sections.get("판례내용") {
        let mut body_cits = extract_statute_citations(body, None);
        statute_cits.append(&mut body_cits);
    }
    // Deduplicate by (law, article, sub, paragraph, item).
    statute_cits.sort_by(|a, b| {
        (
            a.law_name.as_str(),
            a.article,
            a.article_sub,
            a.paragraph,
            a.item,
        )
            .cmp(&(
                b.law_name.as_str(),
                b.article,
                b.article_sub,
                b.paragraph,
                b.item,
            ))
    });
    statute_cits.dedup_by(|a, b| {
        a.law_name == b.law_name
            && a.article == b.article
            && a.article_sub == b.article_sub
            && a.paragraph == b.paragraph
            && a.item == b.item
    });

    let mut case_cits = sections
        .get("참조판례")
        .map(|s| extract_case_numbers(s))
        .unwrap_or_default();
    if let Some(body) = sections.get("판례내용") {
        let mut body_cases = extract_case_numbers(body);
        case_cits.append(&mut body_cases);
    }
    // Don't self-link.
    case_cits.retain(|c| c.case_number != case_number);
    case_cits.sort_by(|a, b| a.case_number.cmp(&b.case_number));
    case_cits.dedup_by(|a, b| a.case_number == b.case_number);

    Ok(CaseDoc {
        slug: case_slug(&case_number),
        case_number,
        case_name,
        court_name,
        court_type_code: sections.get("법원종류코드").map(|s| s.trim().to_string()),
        case_category_code: sections.get("사건종류코드").map(|s| s.trim().to_string()),
        case_type_name: sections.get("사건종류명").map(|s| s.trim().to_string()),
        precedent_serial_no: sections
            .get("판례정보일련번호")
            .map(|s| s.trim().to_string())
            .or(path_meta.precedent_serial_no.clone()),
        verdict_date,
        verdict_kind: sections.get("선고").map(|s| s.trim().to_string()),
        verdict_type: sections.get("판결유형").map(|s| s.trim().to_string()),
        holding: sections.get("판시사항").map(|s| s.to_string()),
        summary: sections.get("판결요지").map(|s| s.to_string()),
        body: sections.get("판례내용").map(|s| s.to_string()),
        source_path: source_path.to_string(),
        statute_citations: statute_cits,
        case_citations: case_cits,
        original_markdown: md.to_string(),
    })
}

// ───────── Internals ─────────

fn split_into_sections(md: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut current_key: Option<String> = None;
    let mut current_val = String::new();

    for line in md.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            // Commit the previous section.
            if let Some(k) = current_key.take() {
                out.insert(k, std::mem::take(&mut current_val).trim().to_string());
            }
            current_key = Some(rest.trim().to_string());
        } else if current_key.is_some() {
            current_val.push_str(line);
            current_val.push('\n');
        }
    }
    if let Some(k) = current_key.take() {
        out.insert(k, current_val.trim().to_string());
    }
    out
}

#[derive(Debug, Default, Clone)]
struct PathMeta {
    verdict_date: Option<String>,
    case_number: Option<String>,
    case_category_code: Option<String>,
    precedent_serial_no: Option<String>,
    court_name: Option<String>,
    case_name: Option<String>,
}

/// Parse `{date_dir}/{case#}_{catcode}_{kind}_{serial}_{verdict}_{court}_{casename}.md`.
/// Tolerant of extra `_` and Korean filesystem spaces.
fn parse_path_metadata(source_path: &str) -> PathMeta {
    let path = Path::new(source_path);
    let mut meta = PathMeta::default();

    if let Some(parent) = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
        if parent.len() == 8 && parent.chars().all(|c| c.is_ascii_digit()) {
            meta.verdict_date = Some(parent.to_string());
        }
    }

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let parts: Vec<&str> = stem.split('_').map(str::trim).collect();
    // Best-effort positional parse: [사건번호, 사건종류코드, 사건종류, 판례정보일련번호, 판결유형, 관할법원, 사건명]
    if let Some(v) = parts.first() {
        if !v.is_empty() {
            meta.case_number = Some(v.to_string());
        }
    }
    if let Some(v) = parts.get(1) {
        if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) {
            meta.case_category_code = Some(v.to_string());
        }
    }
    if let Some(v) = parts.get(3) {
        if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) {
            meta.precedent_serial_no = Some(v.to_string());
        }
    }
    if let Some(v) = parts.get(5) {
        if !v.is_empty() {
            meta.court_name = Some(v.to_string());
        }
    }
    if let Some(v) = parts.get(6) {
        if !v.is_empty() {
            meta.case_name = Some(v.to_string());
        }
    }

    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal but realistic fixture based on the user's shared sample.
    const SAMPLE: &str = r#"## 판시사항
 피고인과 甲이 관련 민사사건에서 임의조정을 성립...

## 참조판례


## 사건종류명
형사

## 판결요지
 반의사불벌죄에서 피해자가 처벌을 희망하지 아니하는 의사표시...

## 참조조문
형사소송법 제327조 제6호, 제364조 제6항, 제366조, 근로기준법 제36조, 제109조, 근로자퇴직급여 보장법 제9조 제1항, 제44조

## 선고일자
20250530

## 법원명
수원지법

## 사건명
근로기준법위반·근로자퇴직급여보장법위반

## 판례내용
【피 고 인】 피고인 ... (대법원 2012. 9. 13. 선고 2012도3166 판결 참조) ...

## 사건번호
2024노3424

## 사건종류코드
400102

## 판례정보일련번호
606941

## 선고
선고

## 판결유형
판결 : 환송

## 법원종류코드
400202
"#;

    #[test]
    fn parses_case_headers_and_metadata() {
        let doc = extract_case(SAMPLE, "/x/20250530/2024노3424_400102_형사_606941_판결_환송_수원지방법원_근로기준법위반.md")
            .unwrap();
        assert_eq!(doc.case_number, "2024노3424");
        assert_eq!(doc.slug, "case::2024노3424");
        assert_eq!(doc.verdict_date.as_deref(), Some("20250530"));
        assert_eq!(doc.case_category_code.as_deref(), Some("400102"));
        assert_eq!(doc.precedent_serial_no.as_deref(), Some("606941"));
        assert_eq!(doc.court_name.as_deref(), Some("수원지법")); // header wins over filename
        assert_eq!(doc.case_type_name.as_deref(), Some("형사"));
    }

    #[test]
    fn extracts_participating_statutes_from_refjo_block() {
        let doc =
            extract_case(SAMPLE, "/x/20250530/2024노3424_400102_형사_606941_판결.md").unwrap();
        let laws: Vec<_> = doc
            .statute_citations
            .iter()
            .map(|c| (c.law_name.as_str(), c.article))
            .collect();
        // Dedup'd set from the sample's 참조조문.
        assert!(laws.contains(&("형사소송법", 327)));
        assert!(laws.contains(&("형사소송법", 364)));
        assert!(laws.contains(&("형사소송법", 366)));
        assert!(laws.contains(&("근로기준법", 36)));
        assert!(laws.contains(&("근로기준법", 109)));
        assert!(laws.contains(&("근로자퇴직급여 보장법", 9)));
        assert!(laws.contains(&("근로자퇴직급여 보장법", 44)));
    }

    #[test]
    fn extracts_reference_cases_from_body() {
        let doc =
            extract_case(SAMPLE, "/x/20250530/2024노3424_400102_형사_606941_판결.md").unwrap();
        let nums: Vec<_> = doc
            .case_citations
            .iter()
            .map(|c| c.case_number.as_str())
            .collect();
        assert!(nums.contains(&"2012도3166"));
        // Self-case is excluded.
        assert!(!nums.contains(&"2024노3424"));
    }
}
