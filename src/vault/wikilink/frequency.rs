// @Ref: SUMMARY §3 Step 1 — quantitative scoring (TF + heading boost + synonym collapse).

use super::tokens::CompoundToken;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

const HEADING1_BOOST: f32 = 3.0;
const HEADING2_BOOST: f32 = 2.0;
const HEADING3_BOOST: f32 = 1.5;
const FRONTMATTER_BOOST: f32 = 2.5;

/// Stop-word list (domain-neutral; boilerplate filter handles domain-specific).
/// Kept short on purpose — real filtering is `boilerplate_words`.
const STOP_WORDS: &[&str] = &[
    "그리고", "또는", "하지만", "그러나", "그런데", "따라서", "그리하여",
    "the", "a", "an", "of", "to", "in", "and", "or", "for", "is", "was",
    "이", "그", "저", "것", "수", "등", "및", "위", "아래", "때문",
];

/// Load synonym → representative map from `vocabulary_relations`.
/// Only `relation_type = 'synonym'` rows contribute.
pub fn load_synonyms(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare(
        "SELECT word_a, word_b, representative FROM vocabulary_relations
         WHERE relation_type = 'synonym'",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (a, b, rep) = row?;
        let canonical = rep.unwrap_or_else(|| a.clone());
        // alias → rep (both directions so either lookup succeeds)
        map.insert(a, canonical.clone());
        map.insert(b, canonical);
    }
    Ok(map)
}

/// Step 1 output: TF-based keyword scores with heading boost, after
/// collapsing synonyms to their canonical form and substituting
/// compound tokens as single units.
///
/// Compound token spans are blanked out of the text before simple
/// tokenisation so their multi-word phrases don't fragment into parts.
pub fn quantitative_scores(
    markdown: &str,
    compounds: &[CompoundToken],
    synonyms: &HashMap<String, String>,
) -> HashMap<String, f32> {
    let mut scores: HashMap<String, f32> = HashMap::new();

    // 1. Compound tokens (statute citations, case numbers, org names) are
    //    by definition high-signal structural references — weight H1.
    for tok in compounds {
        *scores.entry(tok.canonical.clone()).or_insert(0.0) += HEADING1_BOOST;
    }

    // 2. Mask compound spans so naive tokenisation skips them.
    let masked = mask_spans(markdown, compounds);

    // 3. Walk lines, apply heading boost, tokenise rest.
    let mut in_frontmatter = false;
    for raw_line in masked.lines() {
        let line = raw_line.trim_end();
        if line == "---" {
            in_frontmatter = !in_frontmatter;
            continue;
        }
        let boost = if in_frontmatter {
            FRONTMATTER_BOOST
        } else if let Some(rest) = line.strip_prefix("# ") {
            tokenise_and_add(rest, HEADING1_BOOST, synonyms, &mut scores);
            continue;
        } else if let Some(rest) = line.strip_prefix("## ") {
            tokenise_and_add(rest, HEADING2_BOOST, synonyms, &mut scores);
            continue;
        } else if let Some(rest) = line.strip_prefix("### ") {
            tokenise_and_add(rest, HEADING3_BOOST, synonyms, &mut scores);
            continue;
        } else {
            1.0
        };
        tokenise_and_add(line, boost, synonyms, &mut scores);
    }

    scores
}

fn mask_spans(text: &str, compounds: &[CompoundToken]) -> String {
    if compounds.is_empty() {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0usize;
    for c in compounds {
        if c.span_start < idx {
            continue; // overlap guard (already deduped but defensive)
        }
        out.extend_from_slice(&bytes[idx..c.span_start]);
        // replace with space run of same byte length — preserves offsets for callers
        for _ in 0..(c.span_end - c.span_start) {
            out.push(b' ');
        }
        idx = c.span_end;
    }
    out.extend_from_slice(&bytes[idx..]);
    // SAFETY: only ASCII space replaced; original UTF-8 boundaries preserved.
    String::from_utf8(out).expect("UTF-8 preserved after span mask")
}

fn tokenise_and_add(
    s: &str,
    boost: f32,
    synonyms: &HashMap<String, String>,
    scores: &mut HashMap<String, f32>,
) {
    for token in s
        .split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '.' | ',' | ':' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
                        | '"' | '\'' | '·' | '—' | '–' | '-' | '!' | '?'
                )
        })
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if token.chars().count() < 2 {
            continue;
        }
        let lower = token.to_ascii_lowercase();
        if STOP_WORDS.contains(&lower.as_str()) || STOP_WORDS.contains(&token) {
            continue;
        }
        if token.chars().all(|c| c.is_ascii_digit()) {
            continue; // pure numbers aren't wikilink candidates
        }
        // Strip common Korean particles at the end ("750조와" → "750조")
        // so synonym lookup succeeds across 조사 suffixes.
        let stripped = strip_korean_particles(token);
        let lookup_key = if stripped.is_empty() { token } else { stripped };
        let canonical = synonyms
            .get(lookup_key)
            .cloned()
            .unwrap_or_else(|| lookup_key.to_string());
        *scores.entry(canonical).or_insert(0.0) += boost;
    }
}

/// Strip the most common Korean particles (조사) from a word's tail so that
/// `"민법과"` collapses to `"민법"` for scoring/synonym lookups. Non-exhaustive:
/// focuses on high-frequency postpositions that appear in legal text.
fn strip_korean_particles(word: &str) -> &str {
    const PARTICLES: &[&str] = &[
        "으로써", "으로부터", "에서는", "에서도", "부터", "까지", "으로", "에서",
        "에게", "한테", "와는", "과는", "이라", "라고", "이나", "에도", "에는",
        "라는", "라도",
        "은", "는", "이", "가", "을", "를", "의", "와", "과", "도", "만", "로",
        "에", "다", "이다", "야", "란",
    ];
    for p in PARTICLES {
        if word.len() > p.len() && word.ends_with(p) {
            let base = &word[..word.len() - p.len()];
            if base.chars().count() >= 2 {
                return base;
            }
        }
    }
    word
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::wikilink::tokens::detect_compound_tokens;

    #[test]
    fn heading_boost_applied() {
        let md = "# 민법 제750조\n\n본문 내용은 평범한 텍스트입니다. 민법 제750조";
        let compounds = detect_compound_tokens(md);
        let scores = quantitative_scores(md, &compounds, &HashMap::new());
        let s = scores.get("민법 제750조").copied().unwrap_or(0.0);
        // H1 boost (3.0) + 2 compound occurrences × 2.0 = 7.0
        assert!(s >= 5.0, "expected ≥5 got {s}");
    }

    #[test]
    fn stop_words_excluded() {
        let md = "그리고 또는 하지만";
        let scores = quantitative_scores(md, &[], &HashMap::new());
        assert!(scores.is_empty());
    }

    #[test]
    fn synonym_collapses_to_representative() {
        let md = "750조와 750조는 자주 쓰인다";
        let mut syn = HashMap::new();
        syn.insert("750조".into(), "민법 제750조".into());
        let scores = quantitative_scores(md, &[], &syn);
        assert!(scores.contains_key("민법 제750조"));
        assert!(!scores.contains_key("750조"));
    }

    #[test]
    fn pure_numbers_skipped() {
        let md = "2024 1234 5678";
        let scores = quantitative_scores(md, &[], &HashMap::new());
        assert!(scores.is_empty());
    }

    #[test]
    fn masked_spans_do_not_fragment() {
        let md = "대법원 2026. 2. 2. 선고 2025다12345 판결에서 판시";
        let compounds = detect_compound_tokens(md);
        let scores = quantitative_scores(md, &compounds, &HashMap::new());
        // The compound itself is the keyword; "대법원"/"판결" are NOT separate scored.
        assert!(scores.contains_key("대법원 2026. 2. 2. 선고 2025다12345 판결"));
        assert!(!scores.contains_key("대법원"));
    }
}
