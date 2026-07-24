//! Shared `allowed_users` matching used by every chat channel.

/// Case-sensitivity selector for the allowlist comparison. The chat
/// platform defines which one applies; the helper does not infer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// Exact `==` match.
    Sensitive,
    /// `eq_ignore_ascii_case` — IRC nicks, Matrix MXIDs.
    CaseInsensitive,
}

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

#[must_use]
pub fn is_user_allowed_by(
    allowed: &[String],
    user: &str,
    match_fn: impl Fn(&str, &str) -> bool,
) -> bool {
    if allowed.iter().any(|u| u == "*") {
        return true;
    }
    allowed.iter().any(|entry| match_fn(entry, user))
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

    // --- is_user_allowed_by (caller-provided matcher) ---------------

    #[test]
    fn by_empty_denies_and_wildcard_admits() {
        let eq = |e: &str, u: &str| e == u;
        assert!(!is_user_allowed_by(&[], "alice", eq));
        assert!(is_user_allowed_by(&["*".to_string()], "anyone", eq));
    }

    #[test]
    fn by_email_domain_class() {
        // Mirrors email_channel / gmail_push: "@host" / bare "host" match the
        // whole domain; "user@host" is a full case-insensitive address.
        let matcher = |allowed: &str, email: &str| -> bool {
            let email_lower = email.to_lowercase();
            if allowed.starts_with('@') {
                email_lower.ends_with(&allowed.to_lowercase())
            } else if allowed.contains('@') {
                allowed.eq_ignore_ascii_case(email)
            } else {
                email_lower.ends_with(&format!("@{}", allowed.to_lowercase()))
            }
        };
        let list = vec!["@example.com".to_string(), "boss@corp.io".to_string()];
        assert!(is_user_allowed_by(&list, "anyone@Example.com", matcher));
        assert!(is_user_allowed_by(&list, "BOSS@corp.io", matcher));
        assert!(!is_user_allowed_by(&list, "user@evil.com", matcher));
    }

    #[test]
    fn by_phone_e164_normalized() {
        // Mirrors whatsapp_web E.164 normalization (digits only, leading +).
        let norm = |s: &str| -> String {
            let mut out = String::new();
            let mut chars = s.chars();
            if let Some('+') = chars.clone().next() {
                out.push('+');
                chars.next();
            }
            out.extend(chars.filter(|c| c.is_ascii_digit()));
            out
        };
        let matcher = |entry: &str, phone: &str| norm(entry) == norm(phone);
        let list = vec!["+1-555-0100".to_string()];
        assert!(is_user_allowed_by(&list, "+1 555 0100", matcher));
        assert!(!is_user_allowed_by(&list, "+15550101", matcher));
    }

    #[test]
    fn by_wildcard_short_circuits_matcher() {
        let list = vec!["*".to_string()];

        assert!(is_user_allowed_by(&list, "alice", |_, _| {
            panic!("wildcard should short-circuit before custom matcher runs");
        }));
    }
}
