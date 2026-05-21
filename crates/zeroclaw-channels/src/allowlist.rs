//! Shared `allowed_users` matching used by every chat channel.
//!
//! Each channel (Slack, Discord, IRC, Telegram, Matrix, …) carries an
//! `allowed_users: Vec<String>` allowlist with the same semantics:
//!
//! - `["*"]` (or any list containing `"*"`) means "anyone".
//! - Empty list means "deny everyone" (channel is on but no inbound is
//!   accepted yet — matches the "configured but not opened" stance the
//!   channel docs use).
//! - Otherwise, exact match against the user's identifier wins.
//!
//! IRC nicks are case-insensitive per RFC 2812; Matrix MXIDs are also
//! case-insensitive. Most other channels (Slack user IDs, Discord
//! snowflakes, Telegram usernames) are case-sensitive. The
//! [`Match::Sensitive`] / [`Match::CaseInsensitive`] selector encodes
//! that per-channel choice without growing a parallel impl.

/// Case-sensitivity selector for the allowlist comparison. The chat
/// platform defines which one applies; the helper does not infer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// Exact `==` match.
    Sensitive,
    /// `eq_ignore_ascii_case` — IRC nicks, Matrix MXIDs.
    CaseInsensitive,
}

/// Return `true` when `user` is allowed under `allowed`.
///
/// Single source of truth for the per-channel `is_user_allowed` checks.
/// Callers spell their channel's case-sensitivity by passing the
/// matching [`Match`] variant; the helper handles the wildcard, empty,
/// and per-entry comparisons identically across every channel.
#[must_use]
pub fn is_user_allowed(allowed: &[String], user: &str, mode: Match) -> bool {
    if allowed.iter().any(|u| u == "*") {
        return true;
    }
    match mode {
        Match::Sensitive => allowed.iter().any(|u| u == user),
        Match::CaseInsensitive => allowed.iter().any(|u| u.eq_ignore_ascii_case(user)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_allows_anyone() {
        let list = vec!["*".to_string()];
        assert!(is_user_allowed(&list, "alice", Match::Sensitive));
        assert!(is_user_allowed(&list, "ALICE", Match::Sensitive));
    }

    #[test]
    fn empty_list_denies_everyone() {
        assert!(!is_user_allowed(&[], "alice", Match::Sensitive));
        assert!(!is_user_allowed(&[], "alice", Match::CaseInsensitive));
    }

    #[test]
    fn exact_match_case_sensitive() {
        let list = vec!["alice".to_string()];
        assert!(is_user_allowed(&list, "alice", Match::Sensitive));
        assert!(!is_user_allowed(&list, "Alice", Match::Sensitive));
    }

    #[test]
    fn exact_match_case_insensitive() {
        let list = vec!["Alice".to_string()];
        assert!(is_user_allowed(&list, "alice", Match::CaseInsensitive));
        assert!(is_user_allowed(&list, "ALICE", Match::CaseInsensitive));
    }
}
