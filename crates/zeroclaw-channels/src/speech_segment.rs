//! Sentence/paragraph segmentation for streaming text-to-speech.
//!
//! This is the producer half of the voice pipeline: it turns assistant text
//! into an ordered list of speakable chunks. It performs **no I/O** and knows
//! nothing about transports, so the same segmentation feeds every sink:
//!
//! * Telegram — synthesize each chunk as it lands, accumulate the audio, and
//!   re-mux the whole turn into a single voice note.
//! A future live-call sink can use the same boundaries without implementing
//! another text parser. This module does not implement a call transport.
//!
//! The chunking rule is deliberately simple and predictable:
//!
//! 1. The **first** chunk is a single sentence, grown until it clears
//!    [`SpeechSegmentConfig::min_first_chars`] so a "Yes." or "Done!" opener
//!    does not burn a synthesis round-trip on two words. It is hard-capped at
//!    [`SpeechSegmentConfig::max_first_chars`].
//! 2. Every **later** chunk is one paragraph (a non-empty line). Paragraphs
//!    longer than [`SpeechSegmentConfig::max_chars`] are re-packed at sentence
//!    boundaries, and a single over-long sentence is split on whitespace.
//!
//! Getting the first chunk out early is the whole point: time-to-first-audio
//! dominates perceived latency, and it costs nothing to synthesize the
//! remainder while the opener is already playing.

/// Tuning for [`segment_for_speech`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechSegmentConfig {
    /// Minimum character length of the opening chunk. Short opener sentences
    /// are rolled forward until this is met, so the first synthesis call is
    /// never wasted on a one-word fragment.
    pub min_first_chars: usize,
    /// Hard upper bound on the opening chunk, so a long first sentence cannot
    /// defeat the latency win the first-chunk rule exists for.
    pub max_first_chars: usize,
    /// Maximum characters for every chunk after the first.
    pub max_chars: usize,
}

impl Default for SpeechSegmentConfig {
    fn default() -> Self {
        Self {
            min_first_chars: 60,
            max_first_chars: 280,
            max_chars: 1200,
        }
    }
}

impl SpeechSegmentConfig {
    /// Clamp the per-chunk ceiling to a provider's `max_text_length` so a
    /// caller cannot hand synthesis a segment it is guaranteed to reject.
    pub fn with_max_chars(mut self, max_chars: usize) -> Self {
        if max_chars > 0 {
            self.max_chars = self.max_chars.min(max_chars);
            self.max_first_chars = self.max_first_chars.min(max_chars);
            self.min_first_chars = self.min_first_chars.min(self.max_first_chars);
        }
        self
    }
}

/// Split `text` into ordered, speakable chunks. Returns an empty vector for
/// input that contains nothing speakable.
pub fn segment_for_speech(text: &str, cfg: &SpeechSegmentConfig) -> Vec<String> {
    let cleaned = strip_fenced_code(text);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<String> = Vec::new();

    let first_end = bounded_first_end(trimmed, cfg);
    let first = trimmed[..first_end].trim();
    if !first.is_empty() {
        out.push(first.to_string());
    }

    let rest = &trimmed[first_end..];
    for line in rest.split('\n') {
        let block = collapse_whitespace(line);
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        out.extend(split_long(block, cfg.max_chars));
    }

    out
}

fn bounded_first_end(text: &str, cfg: &SpeechSegmentConfig) -> usize {
    let cap = cfg.max_first_chars.max(1).min(cfg.max_chars.max(1));
    let hard_end = text
        .char_indices()
        .nth(cap)
        .map_or(text.len(), |(index, _)| index);
    first_chunk_end(text, cfg.min_first_chars, cap).min(hard_end)
}

/// Stable chunks that can be synthesized before the text stream closes.
/// A sentence needs a following delimiter; a paragraph needs its newline.
/// For an oversized unfinished paragraph only closed chunks are returned.
/// Recomputing this view from the latest snapshot also handles rewrites.
pub(crate) fn ready_segments(text: &str, cfg: &SpeechSegmentConfig, finished: bool) -> Vec<String> {
    if finished {
        return segment_for_speech(text, cfg);
    }
    let cleaned = strip_fenced_code(text);
    let text = cleaned.trim_start();
    if text.is_empty() {
        return Vec::new();
    }
    let first_end = bounded_first_end(text, cfg);
    let first = text[..first_end].trim();
    let has_delimiter = text[first_end..]
        .chars()
        .next()
        .is_some_and(char::is_whitespace);
    let ends_sentence = first
        .trim_end_matches(['"', '\'', ')', ']', '}', '”', '’', '»'])
        .chars()
        .last()
        .is_some_and(is_sentence_terminator);
    let at_cap = text.chars().count() >= cfg.max_first_chars.max(1).min(cfg.max_chars.max(1));
    if first.is_empty()
        || !(has_delimiter && ends_sentence && first.chars().count() >= cfg.min_first_chars
            || at_cap
                && (has_delimiter
                    || first.chars().count()
                        == cfg.max_first_chars.max(1).min(cfg.max_chars.max(1))))
    {
        return Vec::new();
    }
    let mut out = vec![first.to_owned()];
    for line in text[first_end..].split_inclusive('\n') {
        let block = collapse_whitespace(line);
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let mut parts = split_long(block, cfg.max_chars);
        if !line.ends_with('\n') {
            parts.pop();
        }
        out.extend(parts);
    }
    out
}

/// Remove fenced code blocks. Code is never worth speaking: reading braces and
/// identifiers aloud is noise, and it inflates the audio for no benefit.
fn strip_fenced_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        out.push_str(line);
    }
    out
}

/// Collapse runs of interior whitespace to single spaces, preserving the
/// line-level structure that paragraph chunking relies on.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out
}

fn is_sentence_terminator(ch: char) -> bool {
    matches!(ch, '.' | '!' | '?' | '…' | '。' | '！' | '？')
}

/// A terminator only ends a sentence when what follows looks like a boundary.
/// Without this, `3.5` and `v0.8.7` split mid-token and the audio stumbles.
fn is_sentence_boundary_at(text: &str, end: usize) -> bool {
    match text[end..].chars().next() {
        None => true,
        Some(next) => {
            next.is_whitespace() || matches!(next, '"' | '\'' | ')' | ']' | '}' | '”' | '’' | '»')
        }
    }
}

/// Byte index at which the opening chunk should end.
fn first_chunk_end(text: &str, min: usize, max: usize) -> usize {
    let max = max.max(1);
    let mut count = 0usize;
    let mut boundary: Option<usize> = None;
    let mut last_space: Option<usize> = None;
    let mut last_terminator: Option<usize> = None;

    for (idx, ch) in text.char_indices() {
        count += 1;
        if ch.is_whitespace() {
            last_space = Some(idx);
        }
        if is_sentence_terminator(ch) {
            let end = idx + ch.len_utf8();
            last_terminator = Some(end);
            if is_sentence_boundary_at(text, end) {
                boundary = Some(end);
                if count >= min {
                    return extend_over_closers(text, end);
                }
            }
        }
        if count >= max {
            // Prefer the most recent real sentence boundary, then the last
            // whitespace, then a hard cut — in that order of preference.
            if let Some(b) = boundary {
                return extend_over_closers(text, b);
            }
            if let Some(t) = last_terminator
                && text[..t].chars().count() * 4 >= max
            {
                return extend_over_closers(text, t);
            }
            return last_space.filter(|s| *s > 0).unwrap_or(idx + ch.len_utf8());
        }
    }
    text.len()
}

/// Pull trailing closing punctuation/quotes into the chunk so the next chunk
/// does not open with a stray `"` or `)`.
fn extend_over_closers(text: &str, mut end: usize) -> usize {
    while let Some(ch) = text[end..].chars().next() {
        if matches!(ch, '"' | '\'' | ')' | ']' | '}' | '”' | '’' | '»') {
            end += ch.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Split a block into sentences, keeping terminators attached.
fn split_sentences(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (idx, ch) in block.char_indices() {
        if !is_sentence_terminator(ch) {
            continue;
        }
        let end = idx + ch.len_utf8();
        if !is_sentence_boundary_at(block, end) {
            continue;
        }
        let end = extend_over_closers(block, end);
        let piece = block[start..end].trim();
        if !piece.is_empty() {
            out.push(piece.to_string());
        }
        start = end;
    }
    let tail = block[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    if out.is_empty() {
        out.push(block.trim().to_string());
    }
    out
}

/// Pack a paragraph into chunks of at most `max` characters, splitting at
/// sentence boundaries first and on whitespace only as a last resort.
fn split_long(block: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    if block.chars().count() <= max {
        return vec![block.to_string()];
    }

    let mut out = Vec::new();
    let mut cur = String::new();
    for sentence in split_sentences(block) {
        if sentence.chars().count() > max {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            out.extend(hard_split(&sentence, max));
            continue;
        }
        let would_be = if cur.is_empty() {
            sentence.chars().count()
        } else {
            cur.chars().count() + 1 + sentence.chars().count()
        };
        if would_be > max && !cur.trim().is_empty() {
            out.push(cur.trim().to_string());
            cur.clear();
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&sentence);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Last-resort split on whitespace for a single sentence longer than `max`.
fn hard_split(sentence: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in sentence.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > max {
            out.push(cur.trim().to_string());
            cur.clear();
        }
        if word.chars().count() > max {
            // The provider cap is authoritative even without whitespace.
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            for ch in word.chars() {
                if cur.chars().count() == max {
                    out.push(std::mem::take(&mut cur));
                }
                cur.push(ch);
            }
            continue;
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str) -> Vec<String> {
        segment_for_speech(text, &SpeechSegmentConfig::default())
    }

    #[test]
    fn empty_input_yields_no_segments() {
        assert!(seg("").is_empty());
        assert!(seg("   \n\n \t ").is_empty());
    }

    #[test]
    fn short_opener_is_rolled_forward() {
        // "Yes." alone is under min_first_chars, so the next sentence joins it.
        let parts = seg("Yes. I have finished the rebuild and the daemon is running again.");
        assert_eq!(parts.len(), 1, "got {parts:?}");
        assert!(parts[0].starts_with("Yes."));
    }

    #[test]
    fn first_chunk_ends_at_a_sentence_boundary() {
        let parts = seg(
            "The rebuild finished cleanly and the daemon restarted without errors. \
             Next I will verify the voice note upload path end to end.",
        );
        assert_eq!(parts.len(), 2, "got {parts:?}");
        assert!(parts[0].ends_with("errors."));
        assert!(parts[1].starts_with("Next"));
    }

    #[test]
    fn later_chunks_are_paragraphs() {
        let text = "Opening sentence that is long enough to stand on its own as the first chunk of audio.\n\
                    Second paragraph here.\n\
                    Third paragraph here.";
        let parts = seg(text);
        assert_eq!(parts.len(), 3, "got {parts:?}");
        assert_eq!(parts[1], "Second paragraph here.");
        assert_eq!(parts[2], "Third paragraph here.");
    }

    #[test]
    fn decimal_points_do_not_split_sentences() {
        let parts = seg("The latency dropped to 3.5 seconds after the change landed cleanly.");
        assert_eq!(parts.len(), 1, "got {parts:?}");
        assert!(parts[0].contains("3.5"));
    }

    #[test]
    fn version_strings_do_not_split_sentences() {
        let parts = seg("We are running version 0.8.7 on this box and it looks healthy so far.");
        assert_eq!(parts.len(), 1, "got {parts:?}");
        assert!(parts[0].contains("0.8.7"));
    }

    #[test]
    fn fenced_code_is_removed() {
        let parts = seg("Here is the command.\n```\nls -la /home/dev\n```\nThat should be it.");
        let joined = parts.join(" ");
        assert!(!joined.contains("ls -la"), "got {joined:?}");
        assert!(joined.contains("Here is the command."));
    }

    #[test]
    fn long_paragraph_splits_at_sentence_boundaries() {
        let mut text = String::new();
        for i in 0..40 {
            text.push_str(&format!(
                "This is sentence number {i} and it exists to make the paragraph long. "
            ));
        }
        let cfg = SpeechSegmentConfig {
            min_first_chars: 60,
            max_first_chars: 200,
            max_chars: 300,
        };
        let parts = segment_for_speech(&text, &cfg);
        assert!(parts.len() > 3, "got {} parts", parts.len());
        for p in &parts {
            assert!(
                p.chars().count() <= 300,
                "segment over cap ({} chars): {p}",
                p.chars().count()
            );
        }
    }

    #[test]
    fn every_chunk_respects_max_chars() {
        let text = "Supercalifragilisticexpialidocious ".repeat(40);
        let cfg = SpeechSegmentConfig {
            min_first_chars: 20,
            max_first_chars: 50,
            max_chars: 100,
        };
        let parts = segment_for_speech(&text, &cfg);
        assert!(!parts.is_empty());
        for p in &parts {
            assert!(p.chars().count() <= 100, "over cap: {p}");
        }
    }

    #[test]
    fn with_max_chars_clamps_first_chunk_cap() {
        let cfg = SpeechSegmentConfig::default().with_max_chars(100);
        assert_eq!(cfg.max_chars, 100);
        assert_eq!(cfg.max_first_chars, 100);
    }

    #[test]
    fn unbroken_unicode_and_closing_quotes_obey_every_cap() {
        let cfg = SpeechSegmentConfig::default().with_max_chars(100);
        for text in [
            "架".repeat(500),
            format!("{}{}", "An opening sentence. ".repeat(8), "”".repeat(400)),
        ] {
            let parts = segment_for_speech(&text, &cfg);
            assert!(!parts.is_empty());
            assert!(parts.iter().all(|part| part.chars().count() <= 100));
        }
    }

    #[test]
    fn streaming_waits_for_sentence_delimiters_and_paragraph_completion() {
        let cfg = SpeechSegmentConfig::default();
        let opener =
            "The opening sentence is long enough to start synthesis before generation finishes.";
        assert!(ready_segments(opener, &cfg, false).is_empty());
        let text = format!("{opener} The unfinished paragraph");
        assert_eq!(ready_segments(&text, &cfg, false), [opener]);
        let text = format!("{text}\nA final unfinished paragraph");
        assert_eq!(
            ready_segments(&text, &cfg, false),
            [opener, "The unfinished paragraph"]
        );
        assert_eq!(ready_segments(&text, &cfg, true).len(), 3);
    }

    #[test]
    fn streamed_chunks_remain_a_prefix_after_each_character_and_at_finish() {
        let cfg = SpeechSegmentConfig::default().with_max_chars(100);
        for text in [
            format!("A sufficiently long opening sentence which can start audio early.\n{}\nLast paragraph.", "A longer paragraph has several sentences. ".repeat(12)),
            format!("{}\nAfter the long token.", "架".repeat(500)),
            "A sufficiently long opening sentence which can start audio early.\n```rust\nlet hidden = 1;\n```\nVisible again.".into(),
        ] {
            let final_parts = ready_segments(&text, &cfg, true);
            let mut previous = Vec::new();
            for end in text.char_indices().map(|(index, _)| index).chain(std::iter::once(text.len())) {
                let parts = ready_segments(&text[..end], &cfg, false);
                assert!(parts.starts_with(&previous), "stream changed an emitted boundary: {previous:?} -> {parts:?}");
                assert!(final_parts.starts_with(&parts), "stream disagreed with final segmentation: {parts:?}");
                previous = parts;
            }
        }
    }
}
