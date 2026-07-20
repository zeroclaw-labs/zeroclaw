//! Polling state machine: per-(repo, stream) `since` cursors, a bounded
//! seen-ID set shared across transports, and per-repo feed ETags.
//! Provider-agnostic — `channel::listen` drives it, and each provider's
//! `fetch` interprets the stream and cursor against its own API. No IO.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

/// Evict half the dedup set when it reaches this size (same policy as
/// the Twitter channel).
const DEDUP_CAPACITY: usize = 10_000;

/// One endpoint family with its own cursor. Streams advance
/// independently so a busy stream can't push the `since` watermark past
/// items another stream hasn't seen yet. The provider maps each variant
/// onto the appropriate forge endpoint(s).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PollStream {
    /// Opening posts + PR transitions.
    Issues,
    /// Issue/PR comments.
    Comments,
    /// Inline review comments.
    ReviewComments,
    /// Releases.
    Releases,
    /// Workflow/pipeline runs — see [`runs_cursor_candidate`].
    WorkflowRuns,
    /// The repository activity feed (conditional-request backbone).
    Feed,
}

pub struct PollState {
    /// Cold-start floor: events created before this instant are never
    /// processed, so restarts cannot replay history.
    start: DateTime<Utc>,
    /// Per-(repo, stream) high-water mark of processed timestamps.
    cursor: HashMap<(String, PollStream), DateTime<Utc>>,
    /// Recently processed event ids. Shared across every transport —
    /// an item surfaced by both a targeted endpoint and the feed
    /// backbone is admitted exactly once.
    seen: HashSet<String>,
    /// Per-repo ETag of the last feed page (conditional-request backbone).
    etags: HashMap<String, String>,
}

impl PollState {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            start,
            cursor: HashMap::new(),
            seen: HashSet::new(),
            etags: HashMap::new(),
        }
    }

    /// The `since` value to poll this repo's stream with.
    pub fn since(&self, repo_full_name: &str, stream: PollStream) -> DateTime<Utc> {
        self.cursor
            .get(&(repo_full_name.to_string(), stream))
            .copied()
            .unwrap_or(self.start)
    }

    /// Whether an event is fresh: created at-or-after the cold-start floor
    /// and not yet processed. Read-only — call [`Self::mark_seen`] only
    /// once the event has actually been dispatched, so a fetch/dispatch
    /// error cannot advance the dedup set past an undelivered event.
    pub fn is_fresh(&self, id: &str, created_at: DateTime<Utc>) -> bool {
        created_at >= self.start && !self.seen.contains(id)
    }

    /// Record that an event has been dispatched so later ticks dedup it.
    /// Evicts half the set when it reaches capacity (same policy as the
    /// Twitter channel).
    pub fn mark_seen(&mut self, id: &str) {
        if self.seen.len() >= DEDUP_CAPACITY {
            let evict: Vec<String> = self.seen.iter().take(DEDUP_CAPACITY / 2).cloned().collect();
            for key in evict {
                self.seen.remove(&key);
            }
        }
        self.seen.insert(id.to_string());
    }

    /// Advance a stream cursor to cover a processed event. Never moves
    /// backwards.
    pub fn advance(&mut self, repo_full_name: &str, stream: PollStream, at: DateTime<Utc>) {
        let entry = self
            .cursor
            .entry((repo_full_name.to_string(), stream))
            .or_insert(self.start);
        if at > *entry {
            *entry = at;
        }
    }

    /// The ETag to send as `If-None-Match` on this repo's feed request.
    pub fn etag(&self, repo_full_name: &str) -> Option<&str> {
        self.etags.get(repo_full_name).map(String::as_str)
    }

    /// Remember the ETag of a fresh feed page.
    pub fn set_etag(&mut self, repo_full_name: &str, etag: String) {
        self.etags.insert(repo_full_name.to_string(), etag);
    }
}

pub fn runs_cursor_candidate(
    completed_max: Option<DateTime<Utc>>,
    pending_min: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    pending_min.or(completed_max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn state() -> (PollState, DateTime<Utc>) {
        let start = Utc::now();
        (PollState::new(start), start)
    }

    #[test]
    fn since_falls_back_to_start_then_tracks_advance() {
        let (mut s, start) = state();
        assert_eq!(s.since("o/r", PollStream::Comments), start);
        let later = start + Duration::seconds(90);
        s.advance("o/r", PollStream::Comments, later);
        assert_eq!(s.since("o/r", PollStream::Comments), later);
        // Advancing backwards is a no-op.
        s.advance("o/r", PollStream::Comments, start);
        assert_eq!(s.since("o/r", PollStream::Comments), later);
    }

    #[test]
    fn cursors_are_per_stream_and_per_repo() {
        let (mut s, start) = state();
        let later = start + Duration::seconds(100);
        s.advance("a/a", PollStream::Comments, later);
        assert_eq!(s.since("a/a", PollStream::Issues), start);
        assert_eq!(s.since("a/a", PollStream::Releases), start);
        assert_eq!(s.since("b/b", PollStream::Comments), start);
    }

    #[test]
    fn is_fresh_rejects_pre_start_events() {
        let (s, start) = state();
        assert!(!s.is_fresh("ghc_1", start - Duration::seconds(5)));
        assert!(s.is_fresh("ghc_2", start + Duration::seconds(5)));
    }

    #[test]
    fn mark_seen_dedups_repeated_ids_across_transports() {
        let (mut s, start) = state();
        let t = start + Duration::seconds(1);
        // Same underlying object surfaced by a targeted endpoint…
        assert!(s.is_fresh("ghc_1", t));
        s.mark_seen("ghc_1");
        // …and again by the feed backbone: no longer fresh.
        assert!(!s.is_fresh("ghc_1", t));
    }

    #[test]
    fn etags_are_per_repo_and_replaceable() {
        let (mut s, _) = state();
        assert_eq!(s.etag("o/r"), None);
        s.set_etag("o/r", "W/\"abc\"".to_string());
        assert_eq!(s.etag("o/r"), Some("W/\"abc\""));
        assert_eq!(s.etag("other/r"), None);
        s.set_etag("o/r", "W/\"def\"".to_string());
        assert_eq!(s.etag("o/r"), Some("W/\"def\""));
    }

    #[test]
    fn runs_cursor_never_passes_a_pending_run() {
        let now = Utc::now();
        let older = now - Duration::seconds(300);
        // A pending run caps the cursor even when newer runs completed.
        assert_eq!(runs_cursor_candidate(Some(now), Some(older)), Some(older));
        // No pending runs: advance to the newest completed run.
        assert_eq!(runs_cursor_candidate(Some(now), None), Some(now));
        // Nothing new at all: stay put.
        assert_eq!(runs_cursor_candidate(None, None), None);
        // Only pending runs: hold at the earliest one.
        assert_eq!(runs_cursor_candidate(None, Some(older)), Some(older));
    }
}
