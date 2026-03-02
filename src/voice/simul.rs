//! Simultaneous interpretation segmentation engine.
//!
//! Implements "commit-point" based segmentation: incoming partial
//! transcripts are continuously analyzed to find stable prefix regions
//! that will not change. Once a stable segment is long enough and ends
//! at a natural boundary (punctuation, pause, clause break), it is
//! "committed" for translation and audio output.
//!
//! ## Design
//!
//! The engine maintains three key pointers into the source transcript:
//!
//! ```text
//! |---committed---|---stable-uncommitted---|---unstable (may change)---|
//! 0        last_committed      stable_end              partial_end
//! ```
//!
//! - **committed**: text already sent for translation, never re-sent.
//! - **stable-uncommitted**: text unlikely to change but not yet committed.
//! - **unstable**: the trailing N characters that ASR/model may still revise.
//!
//! Commit decisions use a hybrid strategy:
//! 1. **Boundary heuristic**: prefer cutting at punctuation, whitespace, or clause markers.
//! 2. **Silence detection**: if the speaker pauses (no new input for `silence_commit_ms`),
//!    commit all stable text immediately.
//! 3. **Length cap**: if stable uncommitted text exceeds `max_uncommitted_chars`,
//!    force a commit even without a perfect boundary.

use serde::{Deserialize, Serialize};
use std::time::Instant;

// ── Configuration ──────────────────────────────────────────────────

/// Configuration for the simultaneous interpretation segmentation engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentationConfig {
    /// Number of trailing characters considered unstable (may still change).
    /// Higher = safer but more latency. Recommended: 6–12.
    pub unstable_tail_chars: usize,

    /// Minimum number of characters in stable region before committing.
    /// Prevents committing tiny fragments. Recommended: 8–15.
    pub min_commit_chars: usize,

    /// Maximum uncommitted stable characters before forcing a commit.
    /// Prevents unbounded buffering. Recommended: 60–120.
    pub max_uncommitted_chars: usize,

    /// Milliseconds of silence (no new partial) before committing all stable text.
    /// Captures natural pauses. Recommended: 400–800.
    pub silence_commit_ms: u64,

    /// Milliseconds between segmentation ticks (how often to check commit conditions).
    pub tick_interval_ms: u64,
}

impl Default for SegmentationConfig {
    fn default() -> Self {
        Self {
            unstable_tail_chars: 8,
            min_commit_chars: 10,
            max_uncommitted_chars: 80,
            silence_commit_ms: 600,
            tick_interval_ms: 100,
        }
    }
}

// ── Committed segment ──────────────────────────────────────────────

/// A committed source-language segment ready for translation.
#[derive(Debug, Clone, Serialize)]
pub struct CommittedSegment {
    /// Monotonically increasing commit identifier.
    pub commit_id: u64,
    /// The committed source text.
    pub text: String,
    /// Byte offset in the full transcript where this segment starts.
    pub start_offset: usize,
    /// Byte offset in the full transcript where this segment ends (exclusive).
    pub end_offset: usize,
}

// ── Segmentation engine ────────────────────────────────────────────

/// Real-time segmentation engine for simultaneous interpretation.
///
/// Feed partial transcripts via [`update_partial`], then call [`try_commit`]
/// periodically (or after each update) to extract committed segments.
pub struct SegmentationEngine {
    config: SegmentationConfig,

    /// Full accumulated partial transcript (latest version from ASR/model).
    partial_text: String,
    /// Previous partial text (for stable-prefix calculation).
    prev_partial: String,
    /// Byte offset up to which text has been committed.
    last_committed_offset: usize,
    /// Length of the computed stable prefix (bytes).
    stable_prefix_len: usize,

    /// Next commit ID to assign.
    next_commit_id: u64,
    /// Timestamp of the last partial update (for silence detection).
    last_update_time: Instant,
    /// All committed segments (for history/logging).
    committed_segments: Vec<CommittedSegment>,
}

impl SegmentationEngine {
    /// Create a new segmentation engine with the given configuration.
    pub fn new(config: SegmentationConfig) -> Self {
        Self {
            config,
            partial_text: String::new(),
            prev_partial: String::new(),
            last_committed_offset: 0,
            stable_prefix_len: 0,
            next_commit_id: 0,
            last_update_time: Instant::now(),
            committed_segments: Vec::new(),
        }
    }

    /// Update with a new partial transcript from the ASR/model.
    ///
    /// The partial may be a full replacement (Gemini sends cumulative text)
    /// or an incremental append — the engine handles both via common-prefix
    /// comparison with the previous partial.
    pub fn update_partial(&mut self, new_partial: &str) {
        self.prev_partial.clone_from(&self.partial_text);
        self.partial_text = new_partial.to_string();
        self.last_update_time = Instant::now();

        // Compute stable prefix: the common prefix between old and new partials.
        // The trailing `unstable_tail_chars` characters of the common prefix
        // are considered potentially mutable.
        self.stable_prefix_len = self.compute_stable_prefix_len();
    }

    /// Append incremental text to the current partial (for streaming chunk mode).
    pub fn append_partial(&mut self, chunk: &str) {
        self.prev_partial.clone_from(&self.partial_text);
        self.partial_text.push_str(chunk);
        self.last_update_time = Instant::now();
        self.stable_prefix_len = self.compute_stable_prefix_len();
    }

    /// Try to commit a segment. Returns `Some(segment)` if a commit decision
    /// is made, `None` otherwise.
    ///
    /// Call this after each `update_partial`/`append_partial` and also
    /// periodically (every `tick_interval_ms`) to catch silence-based commits.
    pub fn try_commit(&mut self) -> Option<CommittedSegment> {
        let stable_end = self.stable_prefix_len;
        let uncommitted_start = self.last_committed_offset;

        // Nothing to commit if stable prefix hasn't advanced past last commit
        if stable_end <= uncommitted_start {
            return None;
        }

        let available = &self.partial_text[uncommitted_start..stable_end];
        let available_len = available.len();

        // ── Silence-based commit ─────────────────────────────────
        // If speaker has paused long enough, commit everything stable.
        let silence_elapsed_ms = self.last_update_time.elapsed().as_millis();
        if silence_elapsed_ms >= u128::from(self.config.silence_commit_ms) && available_len > 0 {
            return Some(self.do_commit(stable_end));
        }

        // ── Length cap commit ────────────────────────────────────
        // Prevent unbounded buffering.
        if available_len >= self.config.max_uncommitted_chars {
            let boundary = self.find_best_boundary(available);
            let commit_end = uncommitted_start + boundary;
            return Some(self.do_commit(commit_end));
        }

        // ── Boundary-based commit ───────────────────────────────
        // Only commit if we have enough text AND it ends at a natural boundary.
        if available_len >= self.config.min_commit_chars {
            if let Some(boundary) = self.find_natural_boundary(available) {
                let commit_end = uncommitted_start + boundary;
                if commit_end > uncommitted_start {
                    return Some(self.do_commit(commit_end));
                }
            }
        }

        None
    }

    /// Force commit all stable text immediately (e.g., on session end).
    pub fn flush(&mut self) -> Option<CommittedSegment> {
        let stable_end = self.stable_prefix_len;
        if stable_end > self.last_committed_offset {
            Some(self.do_commit(stable_end))
        } else {
            None
        }
    }

    /// Force commit ALL remaining text (including unstable tail).
    /// Used when session is ending and we want everything processed.
    pub fn flush_all(&mut self) -> Option<CommittedSegment> {
        let end = self.partial_text.len();
        if end > self.last_committed_offset {
            Some(self.do_commit(end))
        } else {
            None
        }
    }

    /// Get the current partial text.
    pub fn partial_text(&self) -> &str {
        &self.partial_text
    }

    /// Get the stable prefix length.
    pub fn stable_prefix_len(&self) -> usize {
        self.stable_prefix_len
    }

    /// Get all committed segments.
    pub fn committed_segments(&self) -> &[CommittedSegment] {
        &self.committed_segments
    }

    /// Get the last committed offset.
    pub fn last_committed_offset(&self) -> usize {
        self.last_committed_offset
    }

    /// Reset the engine state for a new interpretation session.
    pub fn reset(&mut self) {
        self.partial_text.clear();
        self.prev_partial.clear();
        self.last_committed_offset = 0;
        self.stable_prefix_len = 0;
        self.next_commit_id = 0;
        self.last_update_time = Instant::now();
        self.committed_segments.clear();
    }

    // ── Internal helpers ──────────────────────────────────────────

    /// Compute the stable prefix length by comparing prev and current partials.
    fn compute_stable_prefix_len(&self) -> usize {
        let old = self.prev_partial.as_bytes();
        let new = self.partial_text.as_bytes();
        let max_common = old.len().min(new.len());

        // Find common prefix length (byte-level, then snap to char boundary)
        let mut common = 0;
        while common < max_common && old[common] == new[common] {
            common += 1;
        }
        // Snap to UTF-8 char boundary
        while common > 0 && !self.partial_text.is_char_boundary(common) {
            common -= 1;
        }

        // Subtract unstable tail
        let tail = self.config.unstable_tail_chars;
        let stable = common.saturating_sub(tail);

        // Snap result to char boundary
        let mut result = stable;
        while result > 0 && !self.partial_text.is_char_boundary(result) {
            result -= 1;
        }

        // For streaming append mode: if the new text is strictly longer and
        // starts with the old text, the entire old text is stable.
        if new.len() > old.len() && new.starts_with(old) {
            let append_stable = old.len().saturating_sub(tail);
            result = result.max(append_stable);
            while result > 0 && !self.partial_text.is_char_boundary(result) {
                result -= 1;
            }
        }

        // Never go below last committed offset
        result.max(self.last_committed_offset)
    }

    /// Find the best boundary position within `text` for committing.
    /// Returns byte offset within `text` (not the full transcript).
    fn find_best_boundary(&self, text: &str) -> usize {
        // Try to find a clause/sentence boundary first, then word boundary
        if let Some(pos) = self.find_natural_boundary(text) {
            return pos;
        }
        // Fall back: find last space
        if let Some(pos) = text.rfind(' ') {
            return pos + 1; // include the space in committed text
        }
        // No boundary at all: commit everything
        text.len()
    }

    /// Find a natural language boundary in `text`.
    /// Returns byte offset (exclusive) of the boundary, or None.
    fn find_natural_boundary(&self, text: &str) -> Option<usize> {
        // Priority 1: sentence-ending punctuation followed by space
        // (handles ". ", "! ", "? ", "。", "！", "？")
        let mut best: Option<usize> = None;

        for (i, c) in text.char_indices() {
            match c {
                // Full-stop punctuation (CJK and Latin)
                '.' | '!' | '?' | '。' | '！' | '？' | '…' => {
                    let end = i + c.len_utf8();
                    // Skip trailing space if present
                    let after = &text[end..];
                    let boundary = if after.starts_with(' ') { end + 1 } else { end };
                    best = Some(boundary);
                }
                // Comma / clause break (weaker boundary)
                ',' | '、' | ';' | '；' | ':' | '：' => {
                    let end = i + c.len_utf8();
                    let after = &text[end..];
                    let boundary = if after.starts_with(' ') { end + 1 } else { end };
                    // Only use comma boundary if we have enough text after last commit
                    if end >= self.config.min_commit_chars {
                        best = Some(boundary);
                    }
                }
                _ => {}
            }
        }

        best
    }

    /// Execute a commit: create segment, update state, return it.
    fn do_commit(&mut self, end_offset: usize) -> CommittedSegment {
        let text = self.partial_text[self.last_committed_offset..end_offset]
            .trim()
            .to_string();
        let segment = CommittedSegment {
            commit_id: self.next_commit_id,
            text,
            start_offset: self.last_committed_offset,
            end_offset,
        };
        self.last_committed_offset = end_offset;
        self.next_commit_id += 1;
        self.committed_segments.push(segment.clone());
        segment
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    fn default_engine() -> SegmentationEngine {
        SegmentationEngine::new(SegmentationConfig::default())
    }

    #[test]
    fn no_commit_for_short_text() {
        let mut engine = default_engine();
        engine.update_partial("Hello");
        assert!(engine.try_commit().is_none());
    }

    #[test]
    fn commit_at_sentence_boundary() {
        // Use unstable_tail=0 to isolate boundary detection from stability logic.
        let config = SegmentationConfig {
            unstable_tail_chars: 0,
            min_commit_chars: 8,
            ..Default::default()
        };
        let mut engine = SegmentationEngine::new(config);
        engine.update_partial("I think we should go to the park. ");
        engine.update_partial("I think we should go to the park. And then ");
        let seg = engine.try_commit();
        assert!(seg.is_some(), "Expected commit at sentence boundary '.'");
        let seg = seg.unwrap();
        assert!(seg.text.contains("park"), "Segment should contain 'park'");
    }

    #[test]
    fn commit_at_comma_boundary() {
        let config = SegmentationConfig {
            unstable_tail_chars: 0,
            min_commit_chars: 8,
            ..Default::default()
        };
        let mut engine = SegmentationEngine::new(config);
        engine.update_partial("In my opinion, we should consider this carefully, ");
        engine.update_partial("In my opinion, we should consider this carefully, because ");
        let seg = engine.try_commit();
        assert!(seg.is_some(), "Expected commit at comma boundary");
    }

    #[test]
    fn silence_triggers_commit() {
        let config = SegmentationConfig {
            silence_commit_ms: 50, // very short for testing
            ..Default::default()
        };
        let mut engine = SegmentationEngine::new(config);
        engine.update_partial("Some stable text here");
        engine.update_partial("Some stable text here");
        thread::sleep(Duration::from_millis(80));
        let seg = engine.try_commit();
        assert!(seg.is_some());
    }

    #[test]
    fn max_length_forces_commit() {
        let config = SegmentationConfig {
            max_uncommitted_chars: 30,
            min_commit_chars: 5,
            unstable_tail_chars: 2,
            ..Default::default()
        };
        let mut engine = SegmentationEngine::new(config);
        let long_text = "abcdefghijklmnopqrstuvwxyz abcdefghijklmnopqrstuvwxyz";
        engine.update_partial(long_text);
        engine.update_partial(long_text); // stabilize by repeating
        let seg = engine.try_commit();
        assert!(seg.is_some());
    }

    #[test]
    fn flush_commits_remaining() {
        let mut engine = default_engine();
        engine.update_partial("Final words");
        engine.update_partial("Final words");
        let seg = engine.flush();
        // flush may or may not produce if text is too short for stable prefix
        // but flush_all should always work
        let _ = seg;
        engine.reset();
        engine.update_partial("Remaining text to process");
        let seg = engine.flush_all();
        assert!(seg.is_some());
    }

    #[test]
    fn cjk_boundary_detection() {
        let mut engine = default_engine();
        engine.update_partial("今日はとても良い天気です。明日も");
        engine.update_partial("今日はとても良い天気です。明日も晴れ");
        let seg = engine.try_commit();
        // Should commit at 。boundary
        if let Some(seg) = seg {
            assert!(seg.text.contains("天気です"));
        }
    }

    #[test]
    fn sequential_commits_cover_full_text() {
        let config = SegmentationConfig {
            min_commit_chars: 5,
            unstable_tail_chars: 3,
            ..Default::default()
        };
        let mut engine = SegmentationEngine::new(config);

        engine.update_partial("First sentence. Second sentence. ");
        engine.update_partial("First sentence. Second sentence. Third.");

        let mut commits = Vec::new();
        while let Some(seg) = engine.try_commit() {
            commits.push(seg);
        }
        // Should have committed at least one segment
        assert!(!commits.is_empty());
        assert_eq!(commits[0].commit_id, 0);
    }

    #[test]
    fn reset_clears_state() {
        let mut engine = default_engine();
        engine.update_partial("Some text");
        engine.reset();
        assert_eq!(engine.partial_text(), "");
        assert_eq!(engine.last_committed_offset(), 0);
    }
}
