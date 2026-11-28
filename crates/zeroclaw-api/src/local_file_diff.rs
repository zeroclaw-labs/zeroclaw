//! Bounded previous-content evidence for an explicitly admitted local tool call.
//!
//! These values are transient and intentionally have no serialization contract.
//! Runtime delivery must keep them out of model output, logs, and history.

use std::fmt;
use std::sync::{Arc, Mutex};

pub const MAX_FILE_DIFF_BYTES: usize = 16 * 1024;
pub const MAX_FILE_DIFF_LINES: usize = 1024;

/// A validated pair of file contents, owned by temporary local delivery.
#[derive(Clone)]
pub struct LocalFileDiff {
    previous: String,
    written: String,
}

impl LocalFileDiff {
    pub fn new(previous: String, written: String) -> Option<Self> {
        let bounded = |text: &str| {
            text.len() <= MAX_FILE_DIFF_BYTES
                && text.lines().count() <= MAX_FILE_DIFF_LINES
                && is_diff_text(text)
        };
        if !bounded(&previous) || !bounded(&written) {
            return None;
        }
        Some(Self { previous, written })
    }

    pub fn previous(&self) -> &str {
        &self.previous
    }

    pub fn written(&self) -> &str {
        &self.written
    }
}

impl fmt::Debug for LocalFileDiff {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalFileDiff")
            .field("previous_bytes", &self.previous.len())
            .field("written_bytes", &self.written.len())
            .finish()
    }
}

/// Reject binary/control text while allowing ordinary line and tab separators.
pub fn is_diff_text(text: &str) -> bool {
    text.chars()
        .all(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
}

/// Per-invocation handoff from a file writer to the runtime's private event path.
#[derive(Clone, Default)]
pub struct LocalFileDiffCapture {
    diff: Arc<Mutex<Option<LocalFileDiff>>>,
}

impl LocalFileDiffCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retain at most one validated pair. Failed or duplicate records are skipped.
    pub fn record(&self, previous: String, written: String) -> bool {
        let Some(diff) = LocalFileDiff::new(previous, written) else {
            return false;
        };
        let Ok(mut slot) = self.diff.lock() else {
            return false;
        };
        if slot.is_some() {
            return false;
        }
        *slot = Some(diff);
        true
    }

    pub fn take(&self) -> Option<LocalFileDiff> {
        self.diff.lock().ok()?.take()
    }
}

tokio::task_local! {
    /// Connection-owned permission, scoped inside a spawned local turn.
    pub static LOCAL_FILE_DIFFS_ALLOWED: bool;

    /// Invocation-owned collector, scoped only after current read admission.
    pub static LOCAL_FILE_DIFF_CAPTURE: Option<LocalFileDiffCapture>;
}

pub fn current_capture() -> Option<LocalFileDiffCapture> {
    LOCAL_FILE_DIFF_CAPTURE
        .try_with(Clone::clone)
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_file_diff_limits_and_controls() {
        assert!(LocalFileDiff::new("x".repeat(MAX_FILE_DIFF_BYTES), String::new()).is_some());
        assert!(LocalFileDiff::new("x".repeat(MAX_FILE_DIFF_BYTES + 1), String::new()).is_none());
        assert!(LocalFileDiff::new(String::new(), "x".repeat(MAX_FILE_DIFF_BYTES + 1)).is_none());
        assert!(LocalFileDiff::new("x\n".repeat(MAX_FILE_DIFF_LINES), String::new()).is_some());
        assert!(LocalFileDiff::new("x\n".repeat(MAX_FILE_DIFF_LINES + 1), String::new()).is_none());
        assert!(LocalFileDiff::new(String::new(), "x\n".repeat(MAX_FILE_DIFF_LINES + 1)).is_none());
        assert!(is_diff_text("a\r\nb\tc"));
        for control in ['\0', '\u{001b}', '\u{007f}', '\u{0085}'] {
            assert!(LocalFileDiff::new(control.to_string(), String::new()).is_none());
            assert!(LocalFileDiff::new(String::new(), control.to_string()).is_none());
        }
    }

    #[test]
    fn local_file_diff_debug_omits_contents() {
        let diff = LocalFileDiff::new("private-previous-value".into(), "new-value".into()).unwrap();
        let debug = format!("{diff:?}");
        assert!(!debug.contains(diff.previous()));
        assert!(!debug.contains(diff.written()));
        assert!(debug.contains("previous_bytes: 22"));
        assert!(debug.contains("written_bytes: 9"));
        let event = crate::agent::TurnEvent::LocalFileDiff {
            id: "write-id".into(),
            diff,
        };
        assert!(!format!("{event:?}").contains("private-previous-value"));
        assert!(!format!("{event:?}").contains("new-value"));
    }

    #[tokio::test]
    async fn local_file_diff_capture_scope_and_single_pair() {
        assert!(current_capture().is_none());
        let capture = LocalFileDiffCapture::new();
        LOCAL_FILE_DIFF_CAPTURE
            .scope(Some(capture.clone()), async {
                let scoped = current_capture().unwrap();
                assert!(!scoped.record("bad\0".into(), "new".into()));
                assert!(scoped.record("old".into(), "new".into()));
                assert!(!scoped.record("other".into(), "replacement".into()));
            })
            .await;
        assert!(current_capture().is_none());
        let diff = capture.take().unwrap();
        assert_eq!(diff.previous(), "old");
        assert_eq!(diff.written(), "new");
        assert!(capture.take().is_none());
    }
}
