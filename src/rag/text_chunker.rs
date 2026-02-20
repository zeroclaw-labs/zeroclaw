//! Text chunker for the RAG pipeline.
//!
//! Wraps [`crate::memory::chunker::chunk_markdown`] to produce [`TextChunk`]s
//! with heading context and configurable chunk/overlap sizes.  Overlap is
//! implemented by prepending the tail of the preceding chunk's content so that
//! retrieval does not miss context at chunk boundaries.

use crate::memory::chunker;

// ── Types ──────────────────────────────────────────────────────────────────────

/// A single chunk of text ready for embedding and storage.
#[derive(Debug, Clone)]
pub struct TextChunk {
    /// The textual content of this chunk.
    pub content: String,
    /// Breadcrumb of section headings (e.g. `"## Residence > ## Character"`).
    pub heading_context: Option<String>,
    /// Zero-based position of this chunk within its source document.
    pub chunk_index: usize,
    /// Approximate token count (1 token ≈ 4 chars).
    pub approximate_tokens: usize,
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Split `text` into [`TextChunk`]s suitable for embedding.
///
/// - `chunk_size_tokens`: target maximum tokens per chunk.
/// - `overlap_tokens`: number of tokens from the end of the previous chunk to
///   prepend to each chunk (for context continuity).
///
/// Splitting respects markdown heading and paragraph boundaries via the
/// existing [`chunker::chunk_markdown`] implementation.  Sentences are never
/// split mid-word.
pub fn chunk_text(text: &str, chunk_size_tokens: usize, overlap_tokens: usize) -> Vec<TextChunk> {
    let base = chunker::chunk_markdown(text, chunk_size_tokens);
    if base.is_empty() {
        return Vec::new();
    }

    let overlap_chars = overlap_tokens.saturating_mul(4);
    let mut result = Vec::with_capacity(base.len());

    for (i, chunk) in base.iter().enumerate() {
        let content = if i > 0 && overlap_chars > 0 {
            let prev = &base[i - 1].content;
            // Find the overlap window; prefer a word boundary.
            let tail = extract_overlap_tail(prev, overlap_chars);
            if tail.is_empty() {
                chunk.content.clone()
            } else {
                format!("{tail}\n{}", chunk.content)
            }
        } else {
            chunk.content.clone()
        };

        let approximate_tokens = content.len() / 4;
        let heading_context = chunk.heading.as_deref().map(str::to_string);

        result.push(TextChunk {
            content,
            heading_context,
            chunk_index: i,
            approximate_tokens,
        });
    }

    result
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Extract the last `max_chars` characters from `text`, snapping forward to
/// the nearest whitespace so we never start in the middle of a word.
fn extract_overlap_tail(text: &str, max_chars: usize) -> &str {
    if text.len() <= max_chars {
        return text;
    }
    let start = text.len() - max_chars;
    // Snap to next whitespace boundary within valid UTF-8.
    let byte_start = text
        .char_indices()
        .skip_while(|(i, _)| *i < start)
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(start);
    text[byte_start..].trim_start()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_returns_no_chunks() {
        assert!(chunk_text("", 500, 50).is_empty());
        assert!(chunk_text("   \n\n", 500, 50).is_empty());
    }

    #[test]
    fn single_short_paragraph_is_one_chunk() {
        let chunks = chunk_text("Hello, world.", 500, 50);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_index, 0);
        assert!(!chunks[0].content.is_empty());
    }

    #[test]
    fn chunk_index_is_sequential() {
        let text = "# A\nContent A\n\n## B\nContent B\n\n## C\nContent C";
        let chunks = chunk_text(text, 500, 0);
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i, "chunk {i} has wrong index");
        }
    }

    #[test]
    fn heading_context_is_preserved() {
        let text = "## Immigration Instructions\nSome policy text here.";
        let chunks = chunk_text(text, 500, 0);
        assert!(!chunks.is_empty());
        let heading = chunks[0].heading_context.as_deref().unwrap_or("");
        assert!(
            heading.contains("Immigration Instructions"),
            "Expected heading context, got: {heading:?}"
        );
    }

    #[test]
    fn approximate_tokens_is_nonzero_for_nonempty_chunk() {
        let chunks = chunk_text("Some content here.", 500, 0);
        assert!(!chunks.is_empty());
        assert!(chunks[0].approximate_tokens > 0);
    }

    #[test]
    fn overlap_prepends_tail_of_previous_chunk() {
        // Build a document large enough to produce at least 2 base chunks.
        use std::fmt::Write as _;
        let mut text = String::from("## Section\n");
        for i in 0..200 {
            let _ = write!(
                text,
                "This is sentence number {i} with some extra words to fill it up properly.\n\n"
            );
        }
        let chunks_with_overlap = chunk_text(&text, 50, 20);
        let chunks_no_overlap = chunk_text(&text, 50, 0);

        if chunks_with_overlap.len() >= 2 && chunks_no_overlap.len() >= 2 {
            // A chunk with overlap should be longer than or equal to the same
            // chunk without (it gained the tail of the previous chunk).
            let with_len = chunks_with_overlap[1].content.len();
            let without_len = chunks_no_overlap[1].content.len();
            assert!(
                with_len >= without_len,
                "overlap chunk ({with_len} chars) should be >= non-overlap ({without_len} chars)"
            );
        }
    }

    #[test]
    fn zero_overlap_produces_clean_chunks() {
        let text = "# Title\nParagraph one.\n\n## Sub\nParagraph two.";
        let chunks = chunk_text(text, 500, 0);
        assert!(!chunks.is_empty());
    }

    #[test]
    fn extract_overlap_tail_short_text_returns_all() {
        let s = "hello world";
        assert_eq!(extract_overlap_tail(s, 100), s);
    }

    #[test]
    fn extract_overlap_tail_snaps_to_whitespace() {
        let s = "one two three four five";
        // max_chars = 10 → starts in middle of "three"; should snap to next space
        let tail = extract_overlap_tail(s, 10);
        assert!(!tail.is_empty());
        // Should not start mid-word
        assert!(
            !tail.starts_with(|c: char| c.is_alphabetic() && !c.is_uppercase()),
            "tail should not start mid-word: {tail:?}"
        );
    }

    #[test]
    fn approximate_tokens_uses_four_chars_per_token() {
        let content = "a".repeat(40);
        let chunks = chunk_text(&content, 500, 0);
        assert!(!chunks.is_empty());
        // 40 chars / 4 = 10 tokens
        assert_eq!(chunks[0].approximate_tokens, 10);
    }

    #[test]
    fn large_document_produces_multiple_chunks() {
        use std::fmt::Write as _;
        let mut text = String::new();
        for i in 0..500 {
            let _ = writeln!(text, "Line {i}: some content here.");
        }
        let chunks = chunk_text(&text, 50, 10);
        assert!(
            chunks.len() > 1,
            "large document should produce multiple chunks, got {}",
            chunks.len()
        );
    }
}
