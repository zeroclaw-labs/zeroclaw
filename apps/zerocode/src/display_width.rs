//! Terminal display-width helpers for TUI layout.
//!
//! `unicode-width` follows Unicode East Asian Width and reports many
//! emoji base characters as width 1 (Ambiguous). Modern terminals render
//! emoji presentation sequences (base + U+FE0F, or emoji-default bases)
//! as 2 cells. Under-counting by 1 cell makes padding/wrapping drop a
//! space immediately after the glyph.
//!
//! These helpers keep East Asian Width for ordinary text and correct
//! emoji presentation sequences to the 2-cell terminal reality.

use unicode_width::UnicodeWidthChar;

/// Display width of a string in terminal cells.
pub(crate) fn display_width(text: &str) -> usize {
    let mut total = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let mut w = ch.width().unwrap_or(0);
        // Emoji presentation selector: force the preceding base to 2 cells.
        // unicode-width already counts FE0F itself as 0, so we only bump the base.
        if chars.peek() == Some(&'\u{FE0F}') && w == 1 {
            w = 2;
        } else if is_emoji_default_presentation(ch) && w == 1 {
            // Emoji-default bases that unicode-width still marks Ambiguous/narrow.
            w = 2;
        }
        total = total.saturating_add(w);
    }
    total
}

/// Display width of a single scalar value in terminal cells.
///
/// Prefer [`display_width`] for multi-codepoint sequences (emoji + VS16,
/// ZWJ families, flags). This is for hard-wrap loops that walk `char`s.
pub(crate) fn char_display_width(ch: char) -> usize {
    let w = ch.width().unwrap_or(0);
    if is_emoji_default_presentation(ch) && w == 1 {
        2
    } else {
        w
    }
}

/// True when `ch` is an emoji-default presentation character that terminals
/// render double-width even without an explicit VS16.
///
/// Covers the common ranges where `unicode-width` still reports 1 for
/// characters that modern terminals treat as emoji (2 cells).
fn is_emoji_default_presentation(ch: char) -> bool {
    let c = ch as u32;
    matches!(
        c,
        // Miscellaneous Symbols and Pictographs, Supplemental Symbols, etc.
        // that are emoji-default but still Ambiguous in East Asian Width.
        0x1F300..=0x1F5FF // Misc Symbols and Pictographs (includes 🏔 U+1F3D4)
            | 0x1F600..=0x1F64F // Emoticons
            | 0x1F680..=0x1F6FF // Transport and Map
            | 0x1F900..=0x1F9FF // Supplemental Symbols and Pictographs
            | 0x1FA70..=0x1FAFF // Symbols and Pictographs Extended-A
            | 0x2600..=0x26FF // Misc symbols (☀ ⚠ ⚡ …) — many are emoji-default
            | 0x2700..=0x27BF // Dingbats
    )
}

/// Sanity: plain ASCII and CJK still match `unicode-width`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_cjk_match_unicode_width() {
        assert_eq!(
            display_width("hello"),
            unicode_width::UnicodeWidthStr::width("hello")
        );
        assert_eq!(
            display_width("界"),
            unicode_width::UnicodeWidthStr::width("界")
        );
        assert_eq!(
            display_width("abcd界"),
            unicode_width::UnicodeWidthStr::width("abcd界")
        );
    }

    #[test]
    fn snow_capped_mountain_with_emoji_presentation_is_two_cells() {
        // 🏔️ = U+1F3D4 + U+FE0F. Terminals draw this as 2 cells; a width of 1
        // is what caused the missing space to the right of the glyph.
        let s = "\u{1F3D4}\u{FE0F}";
        assert_eq!(display_width(s), 2, "emoji presentation must be 2 cells");
        // And the space after it must land one cell further than unicode-width says.
        assert_eq!(display_width(&format!("{s} ")), 3);
    }

    #[test]
    fn snow_capped_mountain_base_without_vs16_is_still_two_cells() {
        // Bare 🏔 is emoji-default in modern terminals even without VS16.
        let s = "\u{1F3D4}";
        assert_eq!(display_width(s), 2);
    }

    #[test]
    fn warning_with_emoji_presentation_is_two_cells() {
        let s = "\u{26A0}\u{FE0F}"; // ⚠️
        assert_eq!(display_width(s), 2);
    }

    #[test]
    fn grinning_face_stays_two() {
        // Already width 2 in unicode-width; helper must not double-count.
        assert_eq!(display_width("\u{1F600}"), 2);
    }

    #[test]
    fn char_display_width_bumps_ambiguous_emoji_base() {
        assert_eq!(char_display_width('\u{1F3D4}'), 2);
        assert_eq!(char_display_width('a'), 1);
        assert_eq!(char_display_width('界'), 2);
    }
}
