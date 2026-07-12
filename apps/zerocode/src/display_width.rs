//! Terminal display-width helpers for TUI layout.
//!
//! Width is delegated to the locked `unicode-width` 0.2 string API, which is
//! sequence-aware for emoji presentation (`base + U+FE0F`), emoji modifiers,
//! flags, and fully-qualified ZWJ sequences. Scalar-only measurement cannot
//! see a following variation selector, so layout paths that walk text must
//! advance by grapheme cluster (or other multi-scalar unit) and measure each
//! unit with [`display_width`].
//!
//! Bare Ambiguous bases without an emoji presentation sequence (for example
//! `U+1F3D4` 🏔 alone) keep the dependency's width of 1. Do not reintroduce
//! block-range overrides for Misc Symbols, Dingbats, or the dedicated emoji
//! blocks — those ranges mix text-default and emoji-default scalars.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
use unicode_width::UnicodeWidthChar;

/// Display width of a string in terminal cells.
///
/// Prefer this over summing per-scalar widths. Sequence-aware cases such as
/// `⚠️` (`U+26A0 U+FE0F`) and `🏔️` (`U+1F3D4 U+FE0F`) are width 2 here even
/// though the base scalar alone is width 1.
pub(crate) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Display width of a single scalar value in terminal cells.
///
/// Prefer [`display_width`] (or grapheme iteration) for multi-codepoint
/// sequences. A scalar helper cannot observe a following `U+FE0F`.
#[cfg(test)]
pub(crate) fn char_display_width(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

/// Iterate extended grapheme clusters of `text` as `(byte_offset, grapheme, width)`.
///
/// Offsets are relative to `text`. Widths come from [`display_width`] so
/// presentation sequences stay intact.
pub(crate) fn grapheme_widths(text: &str) -> impl Iterator<Item = (usize, &str, usize)> + '_ {
    text.grapheme_indices(true)
        .map(|(offset, grapheme)| (offset, grapheme, display_width(grapheme)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_cjk_match_unicode_width() {
        assert_eq!(display_width("hello"), UnicodeWidthStr::width("hello"));
        assert_eq!(display_width("界"), UnicodeWidthStr::width("界"));
        assert_eq!(display_width("abcd界"), UnicodeWidthStr::width("abcd界"));
    }

    #[test]
    fn snow_capped_mountain_with_emoji_presentation_is_two_cells() {
        // 🏔️ = U+1F3D4 + U+FE0F. unicode-width 0.2 string width is already 2;
        // char-wise sum of the base alone is 1 and is what broke layout.
        let s = "\u{1F3D4}\u{FE0F}";
        assert_eq!(UnicodeWidthStr::width(s), 2);
        assert_eq!(display_width(s), 2, "emoji presentation must be 2 cells");
        assert_eq!(display_width(&format!("{s} ")), 3);
        assert_eq!(char_display_width('\u{1F3D4}'), 1);
        assert_eq!(char_display_width('\u{FE0F}'), 0);
    }

    #[test]
    fn snow_capped_mountain_base_without_vs16_keeps_unicode_width() {
        // Bare 🏔 is Ambiguous / not Emoji_Presentation. unicode-width reports
        // width 1; do not force it to 2 via block-range guesses.
        let s = "\u{1F3D4}";
        assert_eq!(display_width(s), UnicodeWidthStr::width(s));
        assert_eq!(display_width(s), 1);
        assert_eq!(char_display_width('\u{1F3D4}'), 1);
    }

    #[test]
    fn thermometer_bare_keeps_unicode_width_without_vs16() {
        // U+1F321 THERMOMETER is another text-default emoji in the same block
        // as the mountain; block-wide overrides would wrongly bump it to 2.
        let s = "\u{1F321}";
        assert_eq!(display_width(s), UnicodeWidthStr::width(s));
        assert_eq!(display_width(s), 1);
        assert_eq!(display_width("\u{1F321}\u{FE0F}"), 2);
    }

    #[test]
    fn warning_with_emoji_presentation_is_two_cells() {
        // ⚠️ is text-default ⚠ + VS16; string-level unicode-width is 2.
        let s = "\u{26A0}\u{FE0F}";
        assert_eq!(UnicodeWidthStr::width(s), 2);
        assert_eq!(display_width(s), 2);
    }

    #[test]
    fn bare_misc_symbol_keeps_unicode_width() {
        // Bare ⚠ (U+26A0) is text-default presentation.
        let s = "\u{26A0}";
        assert_eq!(display_width(s), UnicodeWidthStr::width(s));
        assert_eq!(display_width(s), 1);
        assert_eq!(char_display_width('\u{26A0}'), 1);
    }

    #[test]
    fn bare_dingbat_keeps_unicode_width() {
        // Bare ✓ (U+2713) is a normal width-1 symbol, not emoji-default.
        let s = "\u{2713}";
        assert_eq!(display_width(s), UnicodeWidthStr::width(s));
        assert_eq!(display_width(s), 1);
        assert_eq!(char_display_width('\u{2713}'), 1);
    }

    #[test]
    fn bare_heart_dingbat_keeps_unicode_width_without_vs16() {
        // Bare ❤ (U+2764) is text-default; only ❤️ with VS16 is emoji.
        let s = "\u{2764}";
        assert_eq!(display_width(s), UnicodeWidthStr::width(s));
        assert_eq!(display_width(s), 1);
        assert_eq!(display_width("\u{2764}\u{FE0F}"), 2);
    }

    #[test]
    fn grinning_face_stays_two() {
        // Already width 2 in unicode-width; helper must not double-count.
        assert_eq!(display_width("\u{1F600}"), 2);
    }

    #[test]
    fn sequence_aware_units_include_zwj_and_flags() {
        assert_eq!(display_width("\u{1F1FA}\u{1F1F8}"), 2); // 🇺🇸
        assert_eq!(
            display_width("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"),
            2
        ); // 👨‍👩‍👧
    }

    #[test]
    fn grapheme_widths_keep_presentation_sequences_together() {
        let text = "a\u{26A0}\u{FE0F}b";
        let units: Vec<_> = grapheme_widths(text).collect();
        assert_eq!(units.len(), 3);
        assert_eq!(units[0], (0, "a", 1));
        assert_eq!(units[1].1, "\u{26A0}\u{FE0F}");
        assert_eq!(units[1].2, 2);
        assert_eq!(units[2].1, "b");
        assert_eq!(units[2].2, 1);
    }
}
