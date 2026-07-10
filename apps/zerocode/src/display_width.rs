//! Terminal display-width helpers for TUI layout.
//!
//! `unicode-width` follows Unicode East Asian Width and reports many
//! emoji base characters as width 1 (Ambiguous). Modern terminals render
//! emoji presentation sequences (base + U+FE0F) as 2 cells, and also
//! render emoji-default bases in the dedicated emoji blocks as 2 cells
//! even without an explicit VS16. Under-counting by 1 cell makes
//! padding/wrapping drop a space immediately after the glyph.
//!
//! These helpers keep East Asian Width for ordinary text and correct
//! only the sequences that terminals actually draw double-width.

use unicode_width::UnicodeWidthChar;

/// Display width of a string in terminal cells.
pub(crate) fn display_width(text: &str) -> usize {
    let mut total = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let mut w = ch.width().unwrap_or(0);
        // Explicit emoji presentation selector: force the preceding base
        // to 2 cells. unicode-width already counts FE0F itself as 0.
        if chars.peek() == Some(&'\u{FE0F}') && w == 1 {
            w = 2;
        } else if is_emoji_default_presentation(ch) && w == 1 {
            // Bare emoji-default bases in the dedicated emoji blocks.
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
/// Bare scalars in Misc Symbols / Dingbats keep their `unicode-width`
/// value here; only an explicit `base + U+FE0F` sequence (via
/// [`display_width`]) bumps those to 2.
pub(crate) fn char_display_width(ch: char) -> usize {
    let w = ch.width().unwrap_or(0);
    if is_emoji_default_presentation(ch) && w == 1 {
        2
    } else {
        w
    }
}

/// True when `ch` is an emoji-default presentation character in a
/// dedicated emoji block that terminals render double-width even without
/// an explicit VS16.
///
/// Intentionally **excludes** the broad Misc Symbols (`U+2600..=U+26FF`)
/// and Dingbats (`U+2700..=U+27BF`) ranges: those blocks mix text-default
/// and emoji-default scalars, and bare width-1 symbols there must keep
/// their `unicode-width` value unless an explicit `U+FE0F` follows.
fn is_emoji_default_presentation(ch: char) -> bool {
    let c = ch as u32;
    matches!(
        c,
        // Dedicated emoji blocks. Bases here are emoji-default; terminals
        // draw them 2 cells even without VS16. Includes 🏔 U+1F3D4.
        0x1F300..=0x1F5FF // Misc Symbols and Pictographs
            | 0x1F600..=0x1F64F // Emoticons
            | 0x1F680..=0x1F6FF // Transport and Map
            | 0x1F900..=0x1F9FF // Supplemental Symbols and Pictographs
            | 0x1FA70..=0x1FAFF // Symbols and Pictographs Extended-A
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn ascii_and_cjk_match_unicode_width() {
        assert_eq!(display_width("hello"), UnicodeWidthStr::width("hello"));
        assert_eq!(display_width("界"), UnicodeWidthStr::width("界"));
        assert_eq!(display_width("abcd界"), UnicodeWidthStr::width("abcd界"));
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
        // Bare 🏔 lives in Misc Symbols and Pictographs and is emoji-default.
        let s = "\u{1F3D4}";
        assert_eq!(display_width(s), 2);
    }

    #[test]
    fn warning_with_emoji_presentation_is_two_cells() {
        // ⚠️ is text-default ⚠ + VS16; only the explicit presentation bumps it.
        let s = "\u{26A0}\u{FE0F}";
        assert_eq!(display_width(s), 2);
    }

    #[test]
    fn bare_misc_symbol_keeps_unicode_width() {
        // Bare ⚠ (U+26A0) is text-default presentation. Over-broad range
        // matching used to force this to 2 and would reintroduce off-by-one
        // padding/hit-test bugs in the opposite direction.
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
        // Bare ❤ (U+2764) is text-default; only ❤︎ / ❤️ with VS16 is emoji.
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
    fn char_display_width_bumps_ambiguous_emoji_block_base() {
        assert_eq!(char_display_width('\u{1F3D4}'), 2);
        assert_eq!(char_display_width('a'), 1);
        assert_eq!(char_display_width('界'), 2);
    }
}
