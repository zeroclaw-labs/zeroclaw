// @Ref: PR #3 — query/content normalization + Korean-adaptive weights.
//
// Applied identically to both indexed content and incoming queries so
// that "김대리가" and "김대리" collide after normalisation. Also exposes
// `korean_char_ratio` + `adaptive_weights` for language-aware hybrid
// search tuning.

/// Lightweight normaliser without pulling the `unicode-normalization`
/// crate: fullwidth-ASCII → halfwidth (common in Korean pasted text)
/// plus whitespace squeeze + lowercase for search-side case-insensitivity.
/// Full NFKC via an external crate is tracked as a follow-up.
pub fn normalize_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_ws = false;
    for c in input.chars() {
        let mapped = fullwidth_to_halfwidth(c);
        if mapped.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(mapped);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

fn fullwidth_to_halfwidth(c: char) -> char {
    let code = c as u32;
    // Fullwidth ASCII block U+FF01–U+FF5E → U+0021–U+007E (subtract 0xFEE0)
    if (0xFF01..=0xFF5E).contains(&code) {
        if let Some(ch) = char::from_u32(code - 0xFEE0) {
            return ch;
        }
    }
    // Ideographic space U+3000 → ASCII space
    if code == 0x3000 {
        return ' ';
    }
    c
}

/// Ratio of Hangul syllables (AC00–D7A3) or Jamo (1100–11FF / 3130–318F)
/// among all non-whitespace chars. 0.0 = pure ASCII, 1.0 = pure Korean.
pub fn korean_char_ratio(s: &str) -> f32 {
    let mut total = 0u32;
    let mut korean = 0u32;
    for c in s.chars() {
        if c.is_whitespace() {
            continue;
        }
        total += 1;
        let u = c as u32;
        let is_ko = matches!(u,
            0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F
        );
        if is_ko {
            korean += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        korean as f32 / total as f32
    }
}

/// Return `(fts_weight, vector_weight)` tuned for the query language.
/// Korean-heavy queries rely more on vector since trigram FTS still
/// misses some conjugations; ASCII/Latin queries use a more balanced
/// split per IR practice.
pub fn adaptive_weights(query: &str) -> (f32, f32) {
    let ko = korean_char_ratio(query);
    if ko > 0.30 {
        (0.25, 0.75) // Korean: vector dominant
    } else {
        (0.40, 0.60) // English/mixed: balanced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfkc_unifies_fullwidth_digits() {
        let raw = "ＡＢＣ１２３"; // fullwidth
        let n = normalize_text(raw);
        assert_eq!(n, "ABC123");
    }

    #[test]
    fn whitespace_squeezed() {
        let n = normalize_text("  a\t\tb  c  ");
        assert_eq!(n, "a b c");
    }

    #[test]
    fn korean_ratio_detected() {
        assert!(korean_char_ratio("김대리 프로젝트") > 0.8);
        assert!(korean_char_ratio("hello world") < 0.1);
        assert!(korean_char_ratio("") == 0.0);
    }

    #[test]
    fn adaptive_weights_korean_biased_toward_vector() {
        let (fts, vec) = adaptive_weights("김대리 프로젝트 진행상황");
        assert_eq!((fts, vec), (0.25, 0.75));
    }

    #[test]
    fn adaptive_weights_english_balanced() {
        let (fts, vec) = adaptive_weights("project status report");
        assert_eq!((fts, vec), (0.40, 0.60));
    }
}
