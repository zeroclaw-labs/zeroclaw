// @Ref: SUMMARY §3 Step 0 + §3.1 — Korean-legal compound token regex.

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundTokenKind {
    PrecedentCitation, // 대법원 2026. 2. 2. 선고 2025다12345 판결
    CaseNumber,        // 2024가합12345
    StatuteArticle,    // 민법 제750조 제1항
    Organization,      // ㈜태양에너지, 법무법인(유한) 백상
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompoundToken {
    pub kind: CompoundTokenKind,
    pub canonical: String,
    pub span_start: usize,
    pub span_end: usize,
}

/// Scan markdown for compound tokens defined in SUMMARY §3.1.
///
/// Returns tokens in start-offset order, non-overlapping. Callers should
/// treat the canonical form as a single keyword unit (not split on spaces).
pub fn detect_compound_tokens(markdown: &str) -> Vec<CompoundToken> {
    use std::sync::LazyLock;
    static PRECEDENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"대법원\s*\d{4}\.\s*\d{1,2}\.\s*\d{1,2}\.\s*(?:선고|자|결정)\s*\d{4}[가-힣]+\d+\s*(?:판결|결정)",
        )
        .expect("precedent regex")
    });
    static CASE_NUM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b\d{4}(?:가합|가단|가소|고합|고단|구합|구단|재가합|재고합)\d+\b")
            .expect("case number regex")
    });
    static STATUTE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?:민법|형법|상법|민사소송법|형사소송법|행정소송법|헌법|노동법|노동기준법)\s*제\d+조(?:의\d+)?(?:\s*제\d+항)?(?:\s*제\d+호)?",
        )
        .expect("statute regex")
    });
    static ORG_BRACKET: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"㈜[가-힣A-Za-z0-9]{2,}").expect("org paren regex"));
    static LAW_FIRM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"법무법인(?:\(유한\))?\s*[가-힣A-Za-z]{2,}").expect("law firm regex")
    });

    let mut tokens: Vec<CompoundToken> = Vec::new();
    push_matches(&PRECEDENT, markdown, CompoundTokenKind::PrecedentCitation, &mut tokens);
    push_matches(&CASE_NUM, markdown, CompoundTokenKind::CaseNumber, &mut tokens);
    push_matches(&STATUTE, markdown, CompoundTokenKind::StatuteArticle, &mut tokens);
    push_matches(&ORG_BRACKET, markdown, CompoundTokenKind::Organization, &mut tokens);
    push_matches(&LAW_FIRM, markdown, CompoundTokenKind::Organization, &mut tokens);

    tokens.sort_by_key(|t| t.span_start);

    // Drop overlapping matches — keep the earlier/longer one.
    let mut dedup: Vec<CompoundToken> = Vec::with_capacity(tokens.len());
    for t in tokens {
        if let Some(last) = dedup.last() {
            if t.span_start < last.span_end {
                continue;
            }
        }
        dedup.push(t);
    }
    dedup
}

fn push_matches(re: &Regex, s: &str, kind: CompoundTokenKind, out: &mut Vec<CompoundToken>) {
    for m in re.find_iter(s) {
        let canonical = normalise_whitespace(m.as_str());
        out.push(CompoundToken {
            kind,
            canonical,
            span_start: m.start(),
            span_end: m.end(),
        });
    }
}

fn normalise_whitespace(s: &str) -> String {
    let mut prev_ws = false;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_statute_article_with_clause() {
        let md = "이 사건은 민법 제750조 제1항에 근거합니다.";
        let tokens = detect_compound_tokens(md);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, CompoundTokenKind::StatuteArticle);
        assert_eq!(tokens[0].canonical, "민법 제750조 제1항");
    }

    #[test]
    fn detects_precedent_citation() {
        let md = "참조: 대법원 2026. 2. 2. 선고 2025다12345 판결";
        let tokens = detect_compound_tokens(md);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, CompoundTokenKind::PrecedentCitation);
    }

    #[test]
    fn detects_case_number() {
        let md = "사건번호 2024가합12345 진행 중";
        let tokens = detect_compound_tokens(md);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, CompoundTokenKind::CaseNumber);
        assert_eq!(tokens[0].canonical, "2024가합12345");
    }

    #[test]
    fn detects_law_firm_and_paren_org() {
        let md = "법무법인(유한) 백상과 ㈜태양에너지는 협력 관계";
        let tokens = detect_compound_tokens(md);
        let orgs: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == CompoundTokenKind::Organization)
            .collect();
        assert_eq!(orgs.len(), 2);
    }

    #[test]
    fn non_overlapping_matches() {
        // "민법 제750조" appears inside a sentence; statute shouldn't overlap
        // with a bogus precedent match.
        let md = "민법 제750조와 민법 제751조";
        let tokens = detect_compound_tokens(md);
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(detect_compound_tokens("").is_empty());
    }
}
