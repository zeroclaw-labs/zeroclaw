//! Sticky case-session registry — pin a user's active "case" (사건/안건)
//! so subsequent forwarded messages and questions stay scoped to one
//! memory namespace.
//!
//! ## Flow
//!
//! 1. User issues `/case start 김OO_2024가합123` in their 1:1 chat with MoA.
//! 2. MoA records `(channel, platform_uid) → "김OO_2024가합123"` in this
//!    in-memory store.
//! 3. While the case is active, the gateway derives a `session_id` for
//!    every inbound utterance from the active case label, and passes it to
//!    [`crate::memory::Memory::store`] / `recall` calls so memory is
//!    scoped per-case.
//! 4. `/case end` clears the active case; subsequent messages fall back to
//!    the channel's default session naming.
//!
//! ## Why in-memory?
//!
//! v1 keeps cases ephemeral on purpose. Persistence would require
//! schema work in `auth_store` or a new SQLite table; per CLAUDE.md
//! §3.3 (rule-of-three) we wait until a second consumer needs persisted
//! case state. Memory entries themselves carry the `case_id` tag through
//! the existing `session_id` column, so historical case context survives
//! a restart even if the "currently active" pointer does not.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum length of a user-supplied case label (characters).
///
/// Keeps the stored session_id bounded and the chat reply readable.
const MAX_CASE_LABEL_CHARS: usize = 80;

/// An active case session for a `(channel, platform_uid)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCase {
    /// Sanitized identifier used as the memory `session_id` suffix.
    /// Letters, digits, underscore, hyphen only.
    pub case_id: String,
    /// Original user-supplied label, kept for display in `/case current`.
    pub label: String,
    /// When the user started this case (Unix epoch seconds).
    pub started_at: u64,
}

/// In-memory registry of active cases per `(channel, platform_uid)` pair.
#[derive(Debug, Default)]
pub struct CaseSessionStore {
    inner: RwLock<HashMap<(String, String), ActiveCase>>,
}

impl CaseSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new active case. Replaces any prior case for the same user.
    /// Returns the canonical `ActiveCase` that was stored, or `Err` when
    /// the label is empty or sanitizes to nothing.
    pub fn start(
        &self,
        channel: &str,
        platform_uid: &str,
        label: &str,
    ) -> anyhow::Result<ActiveCase> {
        let label = label.trim();
        if label.is_empty() {
            anyhow::bail!("case label cannot be empty");
        }

        let truncated_label = truncate_chars(label, MAX_CASE_LABEL_CHARS);
        let case_id = sanitize_case_id(&truncated_label);
        if case_id.is_empty() {
            anyhow::bail!("case label produced no valid identifier characters");
        }

        let active = ActiveCase {
            case_id,
            label: truncated_label,
            started_at: epoch_secs(),
        };

        let mut guard = self.inner.write();
        guard.insert(
            (channel.to_string(), platform_uid.to_string()),
            active.clone(),
        );

        Ok(active)
    }

    /// Look up the currently active case, if any.
    pub fn current(&self, channel: &str, platform_uid: &str) -> Option<ActiveCase> {
        let guard = self.inner.read();
        guard
            .get(&(channel.to_string(), platform_uid.to_string()))
            .cloned()
    }

    /// Clear the active case. Returns the case that was cleared, if any.
    pub fn end(&self, channel: &str, platform_uid: &str) -> Option<ActiveCase> {
        let mut guard = self.inner.write();
        guard.remove(&(channel.to_string(), platform_uid.to_string()))
    }

    /// List active cases for a single user across all their channels.
    /// Used by `/case list` to remind the user where they have active cases.
    pub fn list_for_user(&self, platform_uid: &str) -> Vec<(String, ActiveCase)> {
        let guard = self.inner.read();
        guard
            .iter()
            .filter(|((_, uid), _)| uid == platform_uid)
            .map(|((channel, _), active)| (channel.clone(), active.clone()))
            .collect()
    }
}

/// Build the `session_id` to thread through the memory layer.
/// Returns `Some("kakao_case_<case_id>")` when a case is active, `None`
/// otherwise — callers can then fall back to their default session id
/// (e.g. `gateway_message_session_id` for non-case messages).
pub fn case_session_id(channel: &str, case: &ActiveCase) -> String {
    format!("{channel}_case_{}", case.case_id)
}

/// Sanitize a free-form case label into a memory-friendly identifier.
/// Letters, digits, hyphen, and underscore are kept; everything else is
/// collapsed to underscores; consecutive underscores collapsed to one.
fn sanitize_case_id(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut last_was_sep = true;
    for ch in label.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    while out.starts_with('_') {
        out.remove(0);
    }
    out
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_then_current_returns_case() {
        let store = CaseSessionStore::new();
        let active = store
            .start("kakao", "u_1", "김OO_2024가합123")
            .expect("start should succeed");
        assert_eq!(active.label, "김OO_2024가합123");
        assert!(!active.case_id.is_empty());
        assert_eq!(store.current("kakao", "u_1"), Some(active));
    }

    #[test]
    fn empty_label_rejected() {
        let store = CaseSessionStore::new();
        assert!(store.start("kakao", "u_1", "   ").is_err());
        assert!(store.current("kakao", "u_1").is_none());
    }

    #[test]
    fn label_with_only_separators_rejected() {
        let store = CaseSessionStore::new();
        // Punctuation-only collapses to empty case_id and must error out.
        assert!(store.start("kakao", "u_1", "!!! ??? ...").is_err());
    }

    #[test]
    fn end_clears_case_and_returns_prior() {
        let store = CaseSessionStore::new();
        let started = store.start("kakao", "u_1", "사건1").unwrap();
        let ended = store.end("kakao", "u_1");
        assert_eq!(ended, Some(started));
        assert!(store.current("kakao", "u_1").is_none());
    }

    #[test]
    fn end_when_no_active_case_returns_none() {
        let store = CaseSessionStore::new();
        assert!(store.end("kakao", "u_1").is_none());
    }

    #[test]
    fn start_replaces_existing_case() {
        let store = CaseSessionStore::new();
        store.start("kakao", "u_1", "사건A").unwrap();
        let new_active = store.start("kakao", "u_1", "사건B").unwrap();
        assert_eq!(store.current("kakao", "u_1"), Some(new_active.clone()));
        assert_eq!(new_active.label, "사건B");
    }

    #[test]
    fn cases_isolated_across_users_and_channels() {
        let store = CaseSessionStore::new();
        store.start("kakao", "u_1", "사건A").unwrap();
        store.start("kakao", "u_2", "사건B").unwrap();
        store.start("telegram", "u_1", "사건C").unwrap();
        assert_eq!(store.current("kakao", "u_1").unwrap().label, "사건A");
        assert_eq!(store.current("kakao", "u_2").unwrap().label, "사건B");
        assert_eq!(store.current("telegram", "u_1").unwrap().label, "사건C");
    }

    #[test]
    fn list_for_user_returns_all_channels() {
        let store = CaseSessionStore::new();
        store.start("kakao", "u_1", "사건A").unwrap();
        store.start("telegram", "u_1", "사건B").unwrap();
        store.start("kakao", "u_2", "사건C").unwrap();
        let mut cases = store.list_for_user("u_1");
        cases.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].0, "kakao");
        assert_eq!(cases[1].0, "telegram");
    }

    #[test]
    fn label_truncated_to_max_chars() {
        let store = CaseSessionStore::new();
        let very_long = "가".repeat(MAX_CASE_LABEL_CHARS + 50);
        let active = store.start("kakao", "u_1", &very_long).unwrap();
        assert_eq!(active.label.chars().count(), MAX_CASE_LABEL_CHARS);
    }

    #[test]
    fn sanitize_case_id_keeps_unicode_letters_and_collapses_separators() {
        // Korean letters are alphanumeric in Unicode → kept.
        // Spaces and punctuation collapse to a single underscore.
        assert_eq!(sanitize_case_id("김OO 2024가합123"), "김OO_2024가합123");
        assert_eq!(sanitize_case_id("a!!b???c"), "a_b_c");
        assert_eq!(sanitize_case_id("___test___"), "test");
        assert_eq!(sanitize_case_id("  spaces  "), "spaces");
    }

    #[test]
    fn case_session_id_format() {
        let case = ActiveCase {
            case_id: "kim_2024".to_string(),
            label: "kim_2024".to_string(),
            started_at: 0,
        };
        assert_eq!(case_session_id("kakao", &case), "kakao_case_kim_2024");
        assert_eq!(case_session_id("telegram", &case), "telegram_case_kim_2024");
    }

    #[test]
    fn started_at_is_set_to_current_epoch() {
        let store = CaseSessionStore::new();
        let before = epoch_secs();
        let active = store.start("kakao", "u_1", "사건").unwrap();
        let after = epoch_secs();
        assert!(active.started_at >= before && active.started_at <= after);
    }
}
