//! Shared detection types for the prompt-guard and leak-detector scanners.
//!
//! Task 1A: `PromptGuard::detect_prose` and `LeakDetector::detect` expose
//! per-match spans and confidence so the install-screening layer (task 1B) can
//! build a report without re-implementing the pattern set. The legacy
//! `scan()` methods are projections over the same typed results, so their
//! public behavior is unchanged.

use serde::{Deserialize, Serialize};
use std::ops::Range;

/// How strong the pattern match is — *match quality*, not consequence. Kept
/// separate from a finding's impact so the screening layer can weigh a
/// high-confidence-but-benign hit differently from a low-confidence one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DetectionConfidence {
    Low,
    Medium,
    High,
}

/// One detector hit: a stable label, its confidence, the byte range in the
/// scanned text, and a pre-sanitized excerpt safe to display or persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionMatch {
    /// Stable identifier for the matched pattern class, e.g.
    /// `"system_prompt_override"` or `"aws_access_key"`.
    pub label: &'static str,
    pub confidence: DetectionConfidence,
    /// Byte range in the scanned text the match covers.
    pub span: Range<usize>,
    /// Excerpt of the matched text, already sanitized per invariant I10
    /// (control/bidi stripped, capped, credential material redacted).
    pub redacted_excerpt: String,
}

/// Maximum length of a sanitized excerpt (invariant I10).
pub const MAX_EXCERPT_CHARS: usize = 200;

/// Sanitize a snippet for display or persistence (invariant I10): strip ANSI
/// escape introducers, C0/C1 control characters, and Unicode bidi/directional
/// overrides, collapse runs of whitespace, and cap the length. Never returns
/// raw control bytes that could reflow a terminal or hide text.
pub fn sanitize_excerpt(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_EXCERPT_CHARS));
    let mut chars = 0;
    let mut last_was_space = false;
    for ch in raw.chars() {
        if chars >= MAX_EXCERPT_CHARS {
            out.push('…');
            break;
        }
        let stripped = is_bidi_control(ch)
            || is_zero_width(ch)
            || is_tag_or_selector(ch)
            || (ch.is_control() && ch != '\t')
            // C1 controls (U+0080–U+009F) are not flagged by is_control on
            // char, so reject them explicitly.
            || ('\u{80}'..='\u{9f}').contains(&ch);
        if stripped {
            continue;
        }
        let normalized = if ch.is_whitespace() { ' ' } else { ch };
        if normalized == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        out.push(normalized);
        chars += 1;
    }
    out.trim().to_string()
}

/// Unicode bidirectional / directional-override controls (Trojan Source
/// class). Stripped from excerpts and separately flagged by screening.
pub fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{202A}'..='\u{202E}' // LRE, RLE, PDF, LRO, RLO
            | '\u{2066}'..='\u{2069}' // LRI, RLI, FSI, PDI
            | '\u{200E}' | '\u{200F}' // LRM, RLM
    )
}

/// Zero-width / invisible joiner characters used to smuggle text past humans.
pub fn is_zero_width(ch: char) -> bool {
    matches!(
        ch,
        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}'
    )
}

/// Unicode TAG characters (U+E0000–U+E007F) and variation selectors — invisible
/// glyphs that can carry a smuggled instruction channel into a rendered report
/// or a persisted receipt. Stripped from excerpts as defense-in-depth [R3].
pub fn is_tag_or_selector(ch: char) -> bool {
    matches!(
        ch,
        '\u{E0000}'..='\u{E007F}'      // Unicode TAG block
            | '\u{FE00}'..='\u{FE0F}'  // variation selectors
            | '\u{E0100}'..='\u{E01EF}' // variation selectors supplement
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_ansi_and_control() {
        let raw = "hello\x1b[31mworld\x07\u{0000}end";
        let clean = sanitize_excerpt(raw);
        assert!(
            !clean.contains('\x1b'),
            "ANSI introducer survived: {clean:?}"
        );
        assert!(!clean.contains('\x07'));
        assert!(!clean.contains('\u{0}'));
        assert!(clean.contains("hello"));
        assert!(clean.contains("end"));
    }

    #[test]
    fn sanitize_strips_bidi_and_zero_width() {
        let raw = "safe\u{202E}reversed\u{200B}zw";
        let clean = sanitize_excerpt(raw);
        assert!(!clean.chars().any(is_bidi_control));
        assert!(!clean.chars().any(is_zero_width));
        assert!(clean.contains("safe"));
    }

    #[test]
    fn sanitize_strips_tag_chars_and_selectors() {
        let raw = "name\u{E0069}\u{E0067}\u{FE0F}end";
        let clean = sanitize_excerpt(raw);
        assert!(!clean.chars().any(is_tag_or_selector));
        assert!(clean.contains("name"));
        assert!(clean.contains("end"));
    }

    #[test]
    fn sanitize_caps_length() {
        let raw = "a".repeat(500);
        let clean = sanitize_excerpt(&raw);
        // MAX_EXCERPT_CHARS content chars plus the ellipsis marker.
        assert!(clean.chars().count() <= MAX_EXCERPT_CHARS + 1);
        assert!(clean.ends_with('…'));
    }

    #[test]
    fn sanitize_collapses_whitespace() {
        assert_eq!(sanitize_excerpt("a   \n\t  b"), "a b");
    }

    #[test]
    fn confidence_orders_low_to_high() {
        assert!(DetectionConfidence::Low < DetectionConfidence::Medium);
        assert!(DetectionConfidence::Medium < DetectionConfidence::High);
    }
}
