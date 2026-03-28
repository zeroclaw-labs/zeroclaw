//! Message chunking for channels with per-platform character limits.
//!
//! Some messaging platforms reject or silently truncate messages that exceed a
//! maximum length. This module provides [`chunk_message`] to split long text at
//! word boundaries, and per-platform limit constants for the most common cases.
//!
//! # Usage
//!
//! ```rust
//! use zeroclaw::channels::chunker::{chunk_message, TELEGRAM_LIMIT};
//!
//! let long = "word ".repeat(1_000);
//! let chunks = chunk_message(&long, TELEGRAM_LIMIT);
//! assert!(chunks.iter().all(|c| c.len() <= TELEGRAM_LIMIT));
//! ```
//!
//! # Platform limits
//!
//! | Platform   | Constant           | Limit   |
//! |------------|--------------------|---------|
//! | Telegram   | `TELEGRAM_LIMIT`   |  4 096  |
//! | Discord    | `DISCORD_LIMIT`    |  2 000  |
//! | Slack      | `SLACK_LIMIT`      | 40 000  |
//! | Mattermost | `MATTERMOST_LIMIT` | 16 383  |
//! | IRC        | `IRC_LIMIT`        |    400  |
//! | WhatsApp   | `WHATSAPP_LIMIT`   |  4 096  |
//! | Matrix     | `MATRIX_LIMIT`     | 65 535  |
//!
//! Channels that don't need chunking (e.g. email, MQTT) have no constant here.

/// Maximum message length accepted by Telegram.
pub const TELEGRAM_LIMIT: usize = 4_096;

/// Maximum message length accepted by Discord.
pub const DISCORD_LIMIT: usize = 2_000;

/// Practical maximum for Slack messages (Slack's hard limit is higher but
/// very long messages render poorly).
pub const SLACK_LIMIT: usize = 40_000;

/// Mattermost server default maximum post size.
pub const MATTERMOST_LIMIT: usize = 16_383;

/// Safe IRC line limit (conservatively lower than the 512-byte RFC 1459 cap
/// to leave room for prefix and command overhead).
pub const IRC_LIMIT: usize = 400;

/// Maximum message length accepted by WhatsApp.
pub const WHATSAPP_LIMIT: usize = 4_096;

/// Matrix event `body` length limit (soft; very large events may be rejected
/// by some servers).
pub const MATRIX_LIMIT: usize = 65_535;

/// Split `text` into chunks no longer than `max_chars` Unicode scalar values,
/// breaking at word boundaries where possible.
///
/// - If `text.chars().count() <= max_chars` the input is returned as a
///   single-element `Vec`.
/// - Each chunk is at most `max_chars` chars long.
/// - Breaks are inserted at the last whitespace before the limit when one exists
///   in the final third of the chunk window; otherwise a hard break is used.
/// - Leading/trailing whitespace on each chunk is trimmed.
/// - Empty chunks (after trimming) are omitted.
///
/// `max_chars` must be ≥ 1; values < 1 are clamped to 1.
pub fn chunk_message(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);

    if text.chars().count() <= max_chars {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        return vec![trimmed.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text.trim();

    while !remaining.is_empty() {
        let char_count = remaining.chars().count();

        if char_count <= max_chars {
            chunks.push(remaining.trim().to_string());
            break;
        }

        // Find the byte offset of the `max_chars`-th character.
        let split_byte = remaining
            .char_indices()
            .nth(max_chars)
            .map_or(remaining.len(), |(b, _)| b);

        let window = &remaining[..split_byte];

        // Try to find the last whitespace in the trailing third of the window.
        let search_start_byte = {
            let search_start_char = max_chars * 2 / 3;
            window
                .char_indices()
                .nth(search_start_char)
                .map_or(0, |(b, _)| b)
        };

        let break_byte = window[search_start_byte..]
            .rfind(char::is_whitespace)
            .map(|rel| search_start_byte + rel);

        let (chunk, rest) = match break_byte {
            Some(b) => (&remaining[..b], remaining[b..].trim_start()),
            None => (
                &remaining[..split_byte],
                remaining[split_byte..].trim_start(),
            ),
        };

        let chunk = chunk.trim();
        if !chunk.is_empty() {
            chunks.push(chunk.to_string());
        }
        remaining = rest;
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_returned_as_single_chunk() {
        let chunks = chunk_message("hello world", 100);
        assert_eq!(chunks, vec!["hello world"]);
    }

    #[test]
    fn empty_text_returns_empty_vec() {
        assert!(chunk_message("", 100).is_empty());
        assert!(chunk_message("   ", 100).is_empty());
    }

    #[test]
    fn each_chunk_fits_within_limit() {
        let text = "word ".repeat(500); // 2 500 chars
        let chunks = chunk_message(&text, TELEGRAM_LIMIT);
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= TELEGRAM_LIMIT,
                "chunk too long: {} chars",
                chunk.chars().count()
            );
        }
    }

    #[test]
    fn no_content_lost() {
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(200);
        let chunks = chunk_message(&text, DISCORD_LIMIT);
        let rejoined = chunks.join(" ");
        // Whitespace may differ but all words must be present.
        for word in text.split_whitespace() {
            assert!(rejoined.contains(word), "word '{word}' missing from chunks");
        }
    }

    #[test]
    fn breaks_at_word_boundary() {
        // 30 chars of words, limit = 20 → should not cut mid-word.
        let text = "hello world foo bar baz qux quux";
        let chunks = chunk_message(text, 20);
        for chunk in &chunks {
            assert!(!chunk.starts_with(' '));
            assert!(!chunk.ends_with(' '));
        }
    }

    #[test]
    fn very_long_word_hard_breaks() {
        let word = "a".repeat(200);
        let chunks = chunk_message(&word, 50);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 50);
        }
    }

    #[test]
    fn single_char_limit_does_not_panic() {
        let chunks = chunk_message("hello", 1);
        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert_eq!(chunk.chars().count(), 1);
        }
    }

    #[test]
    fn exact_limit_boundary() {
        let text: String = "a".repeat(100);
        let chunks = chunk_message(&text, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 100);
    }

    #[test]
    fn platform_constants_are_sane() {
        const {
            assert!(TELEGRAM_LIMIT > 0);
            assert!(DISCORD_LIMIT > 0);
            assert!(DISCORD_LIMIT < TELEGRAM_LIMIT);
            assert!(IRC_LIMIT < DISCORD_LIMIT);
        }
    }
}
