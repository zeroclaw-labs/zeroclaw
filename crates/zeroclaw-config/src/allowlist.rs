//! Shared sender-allowlist matching primitives.
//!
//! The canonical peer membership state lives in [`crate::schema::Config::peer_groups`].
//! Callers resolve that state at use time and pass the resulting slice here;
//! this module owns matching semantics only and does not cache authorization
//! data.

/// Case-sensitivity selector for an allowlist comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// Exact `==` match.
    Sensitive,
    /// ASCII case-insensitive match.
    CaseInsensitive,
}

/// Return `true` when `user` is allowed under `allowed`.
///
/// A `"*"` entry admits everyone, an empty list denies everyone, and every
/// other list uses the requested per-entry comparison.
#[must_use]
pub fn is_user_allowed(allowed: &[String], user: &str, mode: Match) -> bool {
    if allowed.iter().any(|entry| entry == "*") {
        return true;
    }

    match mode {
        Match::Sensitive => allowed.iter().any(|entry| entry == user),
        Match::CaseInsensitive => allowed.iter().any(|entry| entry.eq_ignore_ascii_case(user)),
    }
}

/// Return `true` when `user` is allowed under `allowed`, using a custom
/// per-entry matcher.
///
/// Wildcard and empty-list behavior remains centralized here, while callers
/// provide only platform-specific identity comparison.
#[must_use]
pub fn is_user_allowed_by(
    allowed: &[String],
    user: &str,
    match_fn: impl Fn(&str, &str) -> bool,
) -> bool {
    if allowed.iter().any(|entry| entry == "*") {
        return true;
    }

    allowed.iter().any(|entry| match_fn(entry, user))
}

/// Compare one email allowlist entry with an address.
///
/// Leading-`@` and bare-domain entries admit an entire domain. Full addresses
/// compare case-insensitively.
#[must_use]
pub fn email_match(allowed: &str, email: &str) -> bool {
    let email_lower = email.to_lowercase();
    if allowed.starts_with('@') {
        email_lower.ends_with(&allowed.to_lowercase())
    } else if allowed.contains('@') {
        allowed.eq_ignore_ascii_case(email)
    } else {
        email_lower.ends_with(&format!("@{}", allowed.to_lowercase()))
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

    #[test]
    fn custom_matcher_keeps_wildcard_and_empty_semantics() {
        let exact = |entry: &str, user: &str| entry == user;
        assert!(!is_user_allowed_by(&[], "alice", exact));
        assert!(is_user_allowed_by(&["*".to_string()], "anyone", exact));
    }

    #[test]
    fn email_match_supports_domains_and_full_addresses() {
        let list = vec!["@example.com".to_string(), "boss@corp.io".to_string()];
        assert!(is_user_allowed_by(&list, "anyone@Example.com", email_match));
        assert!(is_user_allowed_by(&list, "BOSS@corp.io", email_match));
        assert!(!is_user_allowed_by(&list, "user@evil.com", email_match));
    }

    #[test]
    fn custom_matcher_wildcard_short_circuits() {
        assert!(is_user_allowed_by(
            &["*".to_string()],
            "alice",
            |_, _| panic!("wildcard should short-circuit")
        ));
    }
}
